//! Security headers on every response, the 404 fallback included, as long as the
//! layer goes on the outermost router.

use axum::{
    extract::{Request, State},
    http::{
        HeaderName, HeaderValue,
        header::{self, InvalidHeaderValue},
    },
    middleware::Next,
    response::Response,
};

/// A `Content-Security-Policy`, starting strict and opened up one directive at a
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Csp {
    directives: Vec<(String, String)>,
}

impl Default for Csp {
    fn default() -> Self {
        Self::strict()
    }
}

impl Csp {
    /// Same-origin only, with no plugins, no framing, and forms that can only
    /// post back here.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            directives: [
                ("default-src", "'self'"),
                ("base-uri", "'self'"),
                ("form-action", "'self'"),
                ("frame-ancestors", "'none'"),
                ("object-src", "'none'"),
            ]
            .into_iter()
            .map(|(name, sources)| (name.to_owned(), sources.to_owned()))
            .collect(),
        }
    }

    /// Sets one directive, replacing it if present: `set("img-src", "* data:")`.
    #[must_use]
    pub fn set(mut self, directive: &str, sources: &str) -> Self {
        if let Some(entry) = self
            .directives
            .iter_mut()
            .find(|(name, _)| name == directive)
        {
            sources.clone_into(&mut entry.1);
        } else {
            self.directives
                .push((directive.to_owned(), sources.to_owned()));
        }
        self
    }
}

impl std::fmt::Display for Csp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, (name, sources)) in self.directives.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{name} {sources}")?;
        }
        Ok(())
    }
}

/// The headers [`security_headers`] adds.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    csp: HeaderValue,
    hsts: Option<HeaderValue>,
    no_store: bool,
}

impl SecurityHeaders {
    /// Uses `csp`. Responses with no `Cache-Control` of their own get `no-store`,
    /// because pages can hold CSRF tokens or private data.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy holds characters a header can't.
    pub fn new(csp: &Csp) -> Result<Self, InvalidHeaderValue> {
        Ok(Self {
            csp: HeaderValue::from_str(&csp.to_string())?,
            hsts: None,
            no_store: true,
        })
    }

    /// Adds `Strict-Transport-Security`, for apps that are only reached over
    /// HTTPS.
    #[must_use]
    pub fn hsts(mut self, max_age_seconds: u64) -> Self {
        self.hsts = HeaderValue::from_str(&format!("max-age={max_age_seconds}")).ok();
        self
    }

    /// Leaves responses without a `Cache-Control` of their own alone.
    #[must_use]
    pub fn allow_default_caching(mut self) -> Self {
        self.no_store = false;
        self
    }
}

/// Middleware adding the headers. Use it on the outermost router:
///
/// ```ignore
/// router.layer(from_fn_with_state(headers, security_headers))
/// ```
///
/// A handler that sets its own `Content-Security-Policy` or `Cache-Control`
/// keeps it, which lets a route serve something stricter.
pub async fn security_headers(
    State(config): State<SecurityHeaders>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, config.csp);
    }
    if config.no_store && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if let Some(hsts) = config.hsts {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, hsts);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::{Csp, SecurityHeaders};

    #[test]
    fn strict_policy_is_same_origin() {
        assert_eq!(
            Csp::strict().to_string(),
            "default-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'; object-src 'none'"
        );
    }

    #[test]
    fn set_replaces_or_appends() {
        let csp = Csp::strict()
            .set("default-src", "'none'")
            .set("img-src", "* data:")
            .to_string();
        assert!(csp.starts_with("default-src 'none';"), "{csp}");
        assert!(csp.ends_with("; img-src * data:"), "{csp}");
    }

    #[test]
    fn invalid_policy_is_an_error() {
        assert!(SecurityHeaders::new(&Csp::strict().set("x", "a\nb")).is_err());
    }
}
