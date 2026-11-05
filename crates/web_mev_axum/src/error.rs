//! One classification of errors, shared by the HTML and JSON surfaces, so the two
//! always agree and internal failures never leak their source text.

use std::{borrow::Cow, error::Error};

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Serialize;

/// How an app's error type maps onto HTTP.
pub trait WebError: Error + Send + Sync + 'static {
    /// HTTP status of the response.
    fn status(&self) -> StatusCode;

    /// Stable machine-readable code for the JSON surface.
    fn code(&self) -> &'static str;

    /// Text that is safe to send to clients. Internal sources must be redacted.
    fn public_message(&self) -> Cow<'_, str>;

    /// True for unexpected server-side failures, which are logged with their
    /// full internals.
    fn is_internal(&self) -> bool;

    /// Request field that caused the error, when one applies.
    fn field(&self) -> Option<&str> {
        None
    }

    /// Seconds the client should wait before retrying, when that helps.
    fn retry_after(&self) -> Option<u32> {
        None
    }
}

/// The types an app's extractors reject with. Implement it on the app's state.
pub trait ErrorSurface {
    /// Rejection for browser routes.
    type Html: IntoResponse + From<RequestError> + Send;
    /// Rejection for API routes.
    type Json: IntoResponse + From<RequestError> + Send;
}

/// A request the extractors rejected, ready to convert into an app's error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    /// HTTP status of the response.
    pub status: StatusCode,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Text that is safe to send to clients.
    pub message: String,
}

impl RequestError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400: the request is malformed.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// 401: no valid credentials.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    /// 403: authenticated, but not allowed.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// 500 with a generic message. Log the real cause before returning this,
    /// because it doesn't carry one.
    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal server error",
        )
    }

    /// 404: no such route or resource.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "not found")
    }

    /// 405: the route doesn't take this method.
    #[must_use]
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method not allowed",
        )
    }

    /// 413: the body exceeds the route's limit.
    #[must_use]
    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body exceeds the allowed size",
        )
    }

    /// 415: the route doesn't accept this media type.
    #[must_use]
    pub fn unsupported_media_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "unsupported media type",
        )
    }

    /// 422: well-formed input that fails validation.
    #[must_use]
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            message,
        )
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for RequestError {}

impl WebError for RequestError {
    fn status(&self) -> StatusCode {
        self.status
    }

    fn code(&self) -> &'static str {
        self.code
    }

    fn public_message(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.message)
    }

    fn is_internal(&self) -> bool {
        false
    }
}

/// JSON error body of API routes. An app whose protocol crate has its own type
/// must keep the same shape: `{"code", "message", "field"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiErrorBody<'a> {
    /// Stable machine-readable code.
    pub code: &'a str,
    /// Text that is safe to show.
    pub message: &'a str,
    /// Request field that caused the error, or null.
    pub field: Option<&'a str>,
}

fn log_internal<E: WebError>(error: &E) {
    if error.is_internal() {
        tracing::error!(error = %error, "request failed");
    }
}

fn apply_retry_after<E: WebError>(error: &E, headers: &mut HeaderMap) {
    if let Some(seconds) = error.retry_after() {
        headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
}

/// Renders `error` for an API route. A 401 also challenges the client with
/// `WWW-Authenticate: Bearer`.
pub fn json_error_response<E: WebError>(error: &E) -> Response {
    log_internal(error);
    let status = error.status();
    let message = error.public_message();
    let mut response = (
        status,
        Json(ApiErrorBody {
            code: error.code(),
            message: &message,
            field: error.field(),
        }),
    )
        .into_response();
    apply_retry_after(error, response.headers_mut());
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

/// What an HTML error page shows. The fields are already safe to put in markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorPage {
    /// HTTP status of the response.
    pub status: StatusCode,
    /// The status's standard wording.
    pub heading: String,
    /// The public message, HTML-escaped.
    pub message: String,
}

impl ErrorPage {
    /// A complete page with no external assets, for apps with no design of their
    /// own yet. Keep error pages independent of Tera, so they still work when a
    /// template is the thing that failed.
    #[must_use]
    pub fn minimal_html(&self, app_name: &str) -> String {
        format!(
            r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>{code} · {app}</title>
</head>
<body>
  <main>
    <h1>{heading}</h1>
    <p>{message}</p>
    <p><a href="/">Return home</a></p>
  </main>
</body>
</html>"#,
            code = self.status.as_u16(),
            app = escape_html(app_name),
            heading = self.heading,
            message = self.message,
        )
    }
}

/// Renders `error` for a browser route, using `page` for the markup.
pub fn html_error_response<E: WebError>(
    error: &E,
    page: impl FnOnce(&ErrorPage) -> String,
) -> Response {
    log_internal(error);
    let status = error.status();
    let body = page(&ErrorPage {
        status,
        heading: escape_html(status.canonical_reason().unwrap_or("Request failed")),
        message: escape_html(&error.public_message()),
    });
    let mut response = (status, Html(body)).into_response();
    apply_retry_after(error, response.headers_mut());
    response
}

/// Escapes text for HTML content and quoted attribute values.
#[must_use]
pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_markup_and_quotes() {
        assert_eq!(
            escape_html(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
    }
}
