//! In-memory request budgets, per process, over a fixed one-minute window. The
//! global budget also bounds how much the window's maps can grow.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use web_mev_core::{bearer, token_hash};

use crate::{client_ip::TrustedProxies, error::ApiErrorBody};

const WINDOW: Duration = Duration::from_mins(1);

/// Budgets per minute, and which requests they cover.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// All requests.
    pub global: u32,
    /// Requests from one client address.
    pub source: u32,
    /// Requests with one bearer credential.
    pub credential: u32,
    /// Paths that aren't counted. An entry ending in `/` is a prefix, and any
    /// other entry is an exact path.
    pub exempt: Vec<String>,
    /// Only paths starting with this are counted against `credential`. It stops
    /// browser requests paying to hash a header they don't use.
    pub credential_prefix: String,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            global: 6000,
            source: 600,
            credential: 600,
            exempt: vec!["/static/".into(), "/health".into(), "/ready".into()],
            credential_prefix: "/api/".into(),
        }
    }
}

#[derive(Debug)]
struct Window {
    start: Instant,
    total: u32,
    sources: HashMap<IpAddr, u32>,
    credentials: HashMap<String, u32>,
}

impl Window {
    fn new(start: Instant) -> Self {
        Self {
            start,
            total: 0,
            sources: HashMap::new(),
            credentials: HashMap::new(),
        }
    }
}

/// The state of [`enforce_rate_limit`]. Clones share one window.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    limits: Arc<RateLimits>,
    proxies: TrustedProxies,
    window: Arc<Mutex<Window>>,
}

impl RateLimiter {
    /// A limiter counting client addresses resolved through `proxies`.
    #[must_use]
    pub fn new(limits: RateLimits, proxies: TrustedProxies) -> Self {
        Self {
            limits: Arc::new(limits),
            proxies,
            window: Arc::new(Mutex::new(Window::new(Instant::now()))),
        }
    }

    /// Counts one request. On refusal, returns how long until the window resets.
    fn check(
        &self,
        now: Instant,
        source: IpAddr,
        credential: Option<String>,
    ) -> Result<(), Duration> {
        let mut window = self.window.lock().unwrap_or_else(PoisonError::into_inner);
        if now.duration_since(window.start) >= WINDOW {
            *window = Window::new(now);
        }
        let limits = &self.limits;
        if window.total >= limits.global
            || window.sources.get(&source).copied().unwrap_or(0) >= limits.source
            || credential.as_ref().is_some_and(|key| {
                window.credentials.get(key).copied().unwrap_or(0) >= limits.credential
            })
        {
            return Err(WINDOW.saturating_sub(now.duration_since(window.start)));
        }
        window.total += 1;
        *window.sources.entry(source).or_default() += 1;
        if let Some(key) = credential {
            *window.credentials.entry(key).or_default() += 1;
        }
        Ok(())
    }

    fn is_exempt(&self, path: &str) -> bool {
        self.limits.exempt.iter().any(|entry| {
            if entry.ends_with('/') {
                path.starts_with(entry.as_str())
            } else {
                path == entry
            }
        })
    }
}

/// Middleware enforcing the budgets. A refused request gets a 429 with
/// `Retry-After`.
pub async fn enforce_rate_limit(
    State(limiter): State<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if limiter.is_exempt(path) {
        return next.run(request).await;
    }
    let source = limiter
        .proxies
        .resolve(request.extensions(), request.headers());
    let credential = path
        .starts_with(limiter.limits.credential_prefix.as_str())
        .then(|| {
            let authorization = request.headers().get(header::AUTHORIZATION)?;
            bearer(authorization.to_str().ok()).map(token_hash)
        })
        .flatten();
    match limiter.check(Instant::now(), source, credential) {
        Ok(()) => next.run(request).await,
        Err(wait) => refused(wait),
    }
}

fn refused(wait: Duration) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(ApiErrorBody {
            code: "rate_limited",
            message: "request budget exceeded",
            field: None,
        }),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::RETRY_AFTER,
        HeaderValue::from(wait.as_secs().saturating_add(1)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use std::{
        net::IpAddr,
        time::{Duration, Instant},
    };

    use super::{RateLimiter, RateLimits};
    use crate::client_ip::TrustedProxies;

    fn limiter(global: u32, source: u32, credential: u32) -> RateLimiter {
        RateLimiter::new(
            RateLimits {
                global,
                source,
                credential,
                ..RateLimits::default()
            },
            TrustedProxies::default(),
        )
    }

    fn ip(last: u8) -> IpAddr {
        [10, 0, 0, last].into()
    }

    #[test]
    fn budgets_apply_across_sources_and_credentials() {
        let limiter = limiter(3, 100, 1);
        let now = Instant::now();
        assert!(limiter.check(now, ip(1), Some("a".into())).is_ok());
        assert!(limiter.check(now, ip(2), Some("a".into())).is_err());
        assert!(limiter.check(now, ip(3), Some("b".into())).is_ok());
        assert!(limiter.check(now, ip(4), None).is_ok());
        assert!(limiter.check(now, ip(5), None).is_err());
    }

    #[test]
    fn source_budget_is_per_address() {
        let limiter = limiter(100, 1, 100);
        let now = Instant::now();
        assert!(limiter.check(now, ip(1), None).is_ok());
        assert!(limiter.check(now, ip(1), None).is_err());
        assert!(limiter.check(now, ip(2), None).is_ok());
    }

    #[test]
    fn window_resets_and_reports_the_wait() {
        let limiter = limiter(1, 1, 1);
        let start = limiter.window.lock().unwrap().start;
        assert!(limiter.check(start, ip(1), None).is_ok());
        let wait = limiter
            .check(start + Duration::from_secs(10), ip(1), None)
            .unwrap_err();
        assert_eq!(wait, Duration::from_secs(50));
        assert!(
            limiter
                .check(start + Duration::from_mins(1), ip(1), None)
                .is_ok()
        );
    }

    #[test]
    fn exempt_entries_are_prefixes_or_exact() {
        let limiter = limiter(1, 1, 1);
        assert!(limiter.is_exempt("/static/app.css"));
        assert!(limiter.is_exempt("/health"));
        assert!(!limiter.is_exempt("/healthy"));
        assert!(!limiter.is_exempt("/login"));
    }
}
