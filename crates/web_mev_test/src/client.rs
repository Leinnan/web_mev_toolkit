//! A minimal browser over an in-process router: it keeps cookies, remembers the
//! last CSRF token it saw, and provides `ConnectInfo`, which real connections have
//! but `oneshot` requests don't.

use std::{collections::HashMap, net::SocketAddr};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde::Serialize;
use tower::ServiceExt;

/// A decoded response.
#[derive(Debug, Clone)]
pub struct TestResponse {
    /// Status code.
    pub status: StatusCode,
    /// Headers.
    pub headers: HeaderMap,
    /// Body, as lossy UTF-8.
    pub body: String,
}

impl TestResponse {
    /// The `Location` header.
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.header("location")
    }

    /// A header as text.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The body as JSON. Panics if it isn't.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("invalid JSON ({error}): {}", self.body))
    }
}

/// See the module docs.
#[derive(Debug, Clone)]
pub struct Client {
    app: Router,
    cookies: HashMap<String, String>,
    peer: SocketAddr,
    /// The last CSRF token found in a page.
    pub csrf: Option<String>,
}

impl Client {
    /// A client with no cookies, connecting from `127.0.0.1`.
    #[must_use]
    pub fn new(app: Router) -> Self {
        Self {
            app,
            cookies: HashMap::new(),
            peer: SocketAddr::from(([127, 0, 0, 1], 11111)),
            csrf: None,
        }
    }

    /// Connects from another address, such as a proxy or a second visitor.
    #[must_use]
    pub fn from_peer(mut self, peer: SocketAddr) -> Self {
        self.peer = peer;
        self
    }

    /// A cookie's current value.
    #[must_use]
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(String::as_str)
    }

    /// Sets or replaces a cookie.
    pub fn set_cookie(&mut self, name: &str, value: &str) {
        self.cookies.insert(name.to_owned(), value.to_owned());
    }

    /// Forgets a cookie.
    pub fn remove_cookie(&mut self, name: &str) {
        self.cookies.remove(name);
    }

    /// Sends any request.
    pub async fn request(
        &mut self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        if !self.cookies.is_empty() {
            let cookie = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            builder = builder.header(header::COOKIE, cookie);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = builder
            .body(body.map_or_else(Body::empty, Body::from))
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(self.peer));
        let response = self.app.clone().oneshot(request).await.unwrap();
        for value in response.headers().get_all(header::SET_COOKIE) {
            let raw = value.to_str().unwrap();
            let pair = raw.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            if raw.contains("Max-Age=0") || value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(token) = extract_csrf(&body) {
            self.csrf = Some(token);
        }
        TestResponse {
            status,
            headers,
            body,
        }
    }

    /// `GET`.
    pub async fn get(&mut self, uri: &str) -> TestResponse {
        self.request(Method::GET, uri, &[], None).await
    }

    /// `GET`, as htmx sends it.
    pub async fn htmx_get(&mut self, uri: &str) -> TestResponse {
        self.request(Method::GET, uri, &[("hx-request", "true")], None)
            .await
    }

    /// Posts a urlencoded form with the current CSRF token added.
    pub async fn post_form(&mut self, uri: &str, fields: &[(&str, &str)]) -> TestResponse {
        let token = self.csrf.clone().unwrap_or_default();
        let mut all: Vec<(&str, &str)> = vec![("csrf_token", token.as_str())];
        all.extend_from_slice(fields);
        self.post_raw_form(uri, &all).await
    }

    /// Posts a urlencoded form exactly as given.
    pub async fn post_raw_form(&mut self, uri: &str, fields: &[(&str, &str)]) -> TestResponse {
        let body = serde_urlencoded::to_string(fields).unwrap();
        self.request(
            Method::POST,
            uri,
            &[("content-type", "application/x-www-form-urlencoded")],
            Some(body),
        )
        .await
    }

    /// Posts as htmx does: the CSRF token in the header, and `HX-Request` set.
    pub async fn htmx_post(&mut self, uri: &str, fields: &[(&str, &str)]) -> TestResponse {
        let token = self.csrf.clone().unwrap_or_default();
        let body = serde_urlencoded::to_string(fields).unwrap();
        self.request(
            Method::POST,
            uri,
            &[
                ("content-type", "application/x-www-form-urlencoded"),
                ("hx-request", "true"),
                ("x-csrf-token", token.as_str()),
            ],
            Some(body),
        )
        .await
    }

    /// Posts JSON, with extra headers such as `authorization`.
    pub async fn post_json<T: Serialize>(
        &mut self,
        uri: &str,
        headers: &[(&str, &str)],
        body: &T,
    ) -> TestResponse {
        let mut all = vec![("content-type", "application/json")];
        all.extend_from_slice(headers);
        self.request(
            Method::POST,
            uri,
            &all,
            Some(serde_json::to_string(body).unwrap()),
        )
        .await
    }
}

/// Finds a token in a hidden `csrf_token` field or a `csrf-token` meta tag.
fn extract_csrf(body: &str) -> Option<String> {
    for marker in [
        r#"name="csrf_token" value=""#,
        r#"name="csrf-token" content=""#,
    ] {
        if let Some(start) = body.find(marker) {
            let rest = &body[start + marker.len()..];
            let token = &rest[..rest.find('"')?];
            if !token.is_empty() {
                return Some(token.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::extract_csrf;

    #[test]
    fn finds_both_token_forms() {
        assert_eq!(
            extract_csrf(r#"<input name="csrf_token" value="abc">"#).as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_csrf(r#"<meta name="csrf-token" content="def">"#).as_deref(),
            Some("def")
        );
        assert_eq!(extract_csrf("<p>nothing</p>"), None);
        assert_eq!(extract_csrf(r#"<input name="csrf_token" value="">"#), None);
    }
}
