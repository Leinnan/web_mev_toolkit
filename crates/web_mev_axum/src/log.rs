//! One log line per request.

use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::client_ip::TrustedProxies;

/// Settings for [`access_log`].
#[derive(Debug, Clone, Default)]
pub struct AccessLog {
    proxies: TrustedProxies,
    redactions: Vec<(String, usize)>,
}

impl AccessLog {
    /// Logs the client address resolved through `proxies`.
    #[must_use]
    pub fn new(proxies: TrustedProxies) -> Self {
        Self {
            proxies,
            redactions: Vec::new(),
        }
    }

    /// Hides secrets in paths under `prefix`, which ends in `/`: only the first
    /// `keep` segments after it are logged, so `redact("/hook/", 1)` logs
    /// `/hook/abc/secret` as `/hook/abc/…`.
    #[must_use]
    pub fn redact(mut self, prefix: &str, keep: usize) -> Self {
        self.redactions.push((prefix.to_owned(), keep));
        self
    }

    fn loggable_path<'a>(&self, path: &'a str) -> std::borrow::Cow<'a, str> {
        for (prefix, keep) in &self.redactions {
            if let Some(rest) = path.strip_prefix(prefix.as_str()) {
                let mut logged = prefix.clone();
                for segment in rest.split('/').take(*keep) {
                    logged.push_str(segment);
                    logged.push('/');
                }
                logged.push('…');
                return logged.into();
            }
        }
        path.into()
    }
}

/// Middleware logging the client, method, path, status and duration. The query
/// string is never logged, because it can carry secrets. Server errors log at
/// error level.
pub async fn access_log(State(config): State<AccessLog>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = config.loggable_path(request.uri().path()).into_owned();
    let ip = config
        .proxies
        .resolve(request.extensions(), request.headers());
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let elapsed_ms = started.elapsed().as_millis();
    if response.status().is_server_error() {
        tracing::error!(%ip, %method, %path, status, elapsed_ms, "request");
    } else {
        tracing::info!(%ip, %method, %path, status, elapsed_ms, "request");
    }
    response
}

#[cfg(test)]
mod tests {
    use super::AccessLog;

    #[test]
    fn redaction_keeps_the_requested_segments() {
        let log = AccessLog::default()
            .redact("/hook/", 1)
            .redact("/unlock/", 0);
        assert_eq!(log.loggable_path("/hook/abc/secret"), "/hook/abc/…");
        assert_eq!(log.loggable_path("/unlock/secret"), "/unlock/…");
        assert_eq!(log.loggable_path("/other/path"), "/other/path");
        assert_eq!(log.loggable_path("/hook"), "/hook");
    }
}
