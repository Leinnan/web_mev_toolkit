//! Fetching a document: conditional requests, size and time limits, and redirects
//! followed by hand so every hop passes the destination policy.

use std::{net::IpAddr, sync::Arc, time::Duration};

use reqwest::{StatusCode, header, redirect};
use url::{Host, Url};

use crate::policy::{self, PolicyError, PolicyResolver, Resolver, SystemResolver};

const MAX_REDIRECTS: usize = 5;

/// Validators from the previous successful fetch.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Validators {
    /// `ETag` returned last time.
    pub etag: Option<String>,
    /// `Last-Modified` returned last time.
    pub last_modified: Option<String>,
}

/// What one fetch asks for.
#[derive(Debug, Clone, Copy)]
pub struct FetchOptions<'a> {
    /// Largest body accepted, in bytes. The limit applies while reading, so a server
    /// lying about its length can't get past it.
    pub limit: usize,
    /// `Accept` header value.
    pub accept: Option<&'a str>,
    /// Validators for a conditional request.
    pub validators: Option<&'a Validators>,
}

/// A downloaded document.
#[derive(Debug)]
pub struct Fetched {
    /// URL the document was finally served from.
    pub final_url: Url,
    /// Set when every redirect hop was permanent, meaning the document has moved.
    pub moved_to: Option<Url>,
    /// Raw body.
    pub body: Vec<u8>,
    /// `Content-Type` header, if any.
    pub content_type: Option<String>,
    /// Validators for the next conditional request.
    pub validators: Validators,
    /// All response headers, for anything else the app reads, such as `Link`.
    pub headers: header::HeaderMap,
}

/// Result of a fetch that reached the server.
#[derive(Debug)]
pub enum Outcome {
    /// `304 Not Modified`.
    NotModified {
        /// Set when every redirect hop was permanent.
        moved_to: Option<Url>,
    },
    /// A fresh document.
    Fetched(Box<Fetched>),
}

/// Why a fetch failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The URL is malformed or not http(s).
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// The destination resolved only to forbidden addresses.
    #[error(transparent)]
    Blocked(#[from] PolicyError),
    /// The server answered with an error status.
    #[error("server answered HTTP {status}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Seconds from a `Retry-After` header.
        retry_after: Option<u64>,
    },
    /// The document exceeds the limit.
    #[error("response exceeds the {0} byte limit")]
    TooLarge(usize),
    /// More than five redirects.
    #[error("too many redirects")]
    TooManyRedirects,
    /// Connection, TLS or timeout failure. The URL is left out, since it can carry
    /// secrets.
    #[error("request failed: {0}")]
    Network(String),
}

/// Parses a user-supplied URL. A missing scheme defaults to `https://`. Only http(s)
/// URLs with a host pass, embedded credentials are refused, and any fragment is
/// removed.
///
/// # Errors
///
/// Returns [`FetchError::InvalidUrl`] for anything else.
pub fn parse_http_url(raw: &str) -> Result<Url, FetchError> {
    let raw = raw.trim();
    let candidate = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let mut url =
        Url::parse(&candidate).map_err(|error| FetchError::InvalidUrl(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::InvalidUrl(
            "only http and https are supported".into(),
        ));
    }
    if url.host().is_none() {
        return Err(FetchError::InvalidUrl("the URL has no host".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::InvalidUrl(
            "credentials in URLs are not supported".into(),
        ));
    }
    url.set_fragment(None);
    Ok(url)
}

/// Seconds to wait from a `Retry-After` value: delta seconds or an HTTP date. A date
/// in the past means now.
fn parse_retry_after(value: &str, now: std::time::SystemTime) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        return Some(seconds.max(0).unsigned_abs());
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or_default().as_secs())
}

fn header_string(headers: &header::HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// A reqwest client that can only reach what the policy allows. It ignores proxy
/// settings, because a proxy would make the connection itself and bypass the DNS
/// check. Clones share a connection pool.
#[derive(Debug, Clone)]
pub struct SafeClient {
    http: reqwest::Client,
    allow_private: bool,
}

impl SafeClient {
    /// A client with the system DNS resolver. `timeout` covers a whole request.
    ///
    /// # Errors
    ///
    /// Returns the reqwest error when the TLS backend can't start.
    pub fn new(user_agent: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        Self::with_resolver(user_agent, timeout, Arc::new(SystemResolver), false)
    }

    /// A client with another resolver. `allow_private` turns the policy off, for
    /// operators fetching from their own network.
    ///
    /// # Errors
    ///
    /// Returns the reqwest error when the TLS backend can't start.
    pub fn with_resolver(
        user_agent: &str,
        timeout: Duration,
        resolver: Arc<dyn Resolver>,
        allow_private: bool,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_mins(1))
            .user_agent(user_agent)
            .dns_resolver(Arc::new(PolicyResolver::new(resolver, allow_private)))
            .build()?;
        Ok(Self {
            http,
            allow_private,
        })
    }

    /// The underlying client, for requests [`fetch`](Self::fetch) doesn't cover, such
    /// as posting a webhook. Its DNS step enforces the policy, but call
    /// [`check_url`](Self::check_url) first, because IP-literal hosts skip DNS.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Checks a URL whose host is an IP literal. Names are checked by the resolver
    /// when connecting.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError::Blocked`] for a forbidden address.
    pub fn check_url(&self, url: &Url) -> Result<(), FetchError> {
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            Some(Host::Domain(_)) => return Ok(()),
            None => return Err(FetchError::InvalidUrl("the URL has no host".into())),
        };
        if self.allow_private || policy::is_allowed_destination(ip) {
            Ok(())
        } else {
            Err(PolicyError::Blocked.into())
        }
    }

    /// Fetches `url`, following redirects by hand and checking each hop.
    ///
    /// # Errors
    ///
    /// Returns a [`FetchError`] for forbidden destinations, error statuses, oversized
    /// bodies, redirect loops and network failures.
    pub async fn fetch(
        &self,
        url: &Url,
        options: &FetchOptions<'_>,
    ) -> Result<Outcome, FetchError> {
        let mut current = url.clone();
        let mut all_permanent = true;
        for _ in 0..=MAX_REDIRECTS {
            self.check_url(&current)?;
            let mut response = self
                .request(&current, options)
                .send()
                .await
                .map_err(network_error)?;
            let status = response.status();

            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let location = header_string(response.headers(), header::LOCATION)
                    .ok_or_else(|| FetchError::Network("redirect without Location".into()))?;
                let next = current
                    .join(&location)
                    .map_err(|error| FetchError::InvalidUrl(error.to_string()))?;
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(FetchError::InvalidUrl("redirect to a non-http URL".into()));
                }
                all_permanent &= matches!(
                    status,
                    StatusCode::MOVED_PERMANENTLY | StatusCode::PERMANENT_REDIRECT
                );
                current = next;
                continue;
            }

            let moved_to = (all_permanent && current != *url).then(|| current.clone());
            if status == StatusCode::NOT_MODIFIED {
                return Ok(Outcome::NotModified { moved_to });
            }
            if !status.is_success() {
                let retry_after = header_string(response.headers(), header::RETRY_AFTER)
                    .and_then(|value| parse_retry_after(&value, std::time::SystemTime::now()));
                return Err(FetchError::Status {
                    status: status.as_u16(),
                    retry_after,
                });
            }

            let limit = options.limit;
            if response
                .content_length()
                .is_some_and(|length| length > limit as u64)
            {
                return Err(FetchError::TooLarge(limit));
            }
            let headers = response.headers().clone();
            // Sized up front when the length is known, instead of growing by doubling.
            let mut body = Vec::with_capacity(response.content_length().map_or(0, |length| {
                usize::try_from(length).unwrap_or(limit).min(limit)
            }));
            while let Some(chunk) = response.chunk().await.map_err(network_error)? {
                if body.len() + chunk.len() > limit {
                    return Err(FetchError::TooLarge(limit));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Outcome::Fetched(Box::new(Fetched {
                final_url: current,
                moved_to,
                body,
                content_type: header_string(&headers, header::CONTENT_TYPE),
                validators: Validators {
                    etag: header_string(&headers, header::ETAG),
                    last_modified: header_string(&headers, header::LAST_MODIFIED),
                },
                headers,
            })));
        }
        Err(FetchError::TooManyRedirects)
    }

    fn request(&self, url: &Url, options: &FetchOptions<'_>) -> reqwest::RequestBuilder {
        let mut request = self.http.get(url.clone());
        if let Some(accept) = options.accept {
            request = request.header(header::ACCEPT, accept);
        }
        if let Some(validators) = options.validators {
            if let Some(etag) = &validators.etag {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(modified) = &validators.last_modified {
                request = request.header(header::IF_MODIFIED_SINCE, modified);
            }
        }
        request
    }
}

/// Maps a transport error, recognizing destinations the policy refused.
fn network_error(error: reqwest::Error) -> FetchError {
    if let Some(policy) = policy::find_policy_error(&error) {
        return policy.clone().into();
    }
    FetchError::Network(error.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        net::IpAddr,
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };

    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::{
        FetchError, FetchOptions, Outcome, PolicyError, SafeClient, Validators, parse_http_url,
        parse_retry_after,
    };
    use crate::policy::{BoxResolveFuture, Resolver};

    /// Answers every lookup with one address, so tests never touch real DNS.
    struct Pinned(IpAddr);

    impl Resolver for Pinned {
        fn resolve(&self, _host: String, _port: u16) -> BoxResolveFuture {
            let ip = self.0;
            Box::pin(async move { Ok(vec![ip]) })
        }
    }

    fn client(allow_private: bool, ip: &str) -> SafeClient {
        SafeClient::with_resolver(
            "test",
            Duration::from_secs(5),
            Arc::new(Pinned(ip.parse().unwrap())),
            allow_private,
        )
        .unwrap()
    }

    fn options(limit: usize) -> FetchOptions<'static> {
        FetchOptions {
            limit,
            accept: None,
            validators: None,
        }
    }

    fn url(server: &MockServer, route: &str) -> Url {
        Url::parse(&format!("{}{route}", server.uri())).unwrap()
    }

    fn fetched(outcome: Outcome) -> Box<super::Fetched> {
        match outcome {
            Outcome::Fetched(fetched) => fetched,
            Outcome::NotModified { .. } => panic!("expected a document"),
        }
    }

    #[tokio::test]
    async fn downloads_a_document_with_its_validators() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/doc"))
            .and(header("accept", "text/plain"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "\"abc\"")
                    .insert_header("last-modified", "yesterday")
                    .insert_header("content-type", "text/plain")
                    .insert_header("link", "<https://hub.example/>; rel=hub")
                    .set_body_string("hello"),
            )
            .mount(&server)
            .await;
        let document = fetched(
            client(true, "127.0.0.1")
                .fetch(
                    &url(&server, "/doc"),
                    &FetchOptions {
                        accept: Some("text/plain"),
                        ..options(100)
                    },
                )
                .await
                .unwrap(),
        );
        assert_eq!(document.body, b"hello");
        assert_eq!(document.content_type.as_deref(), Some("text/plain"));
        assert_eq!(document.validators.etag.as_deref(), Some("\"abc\""));
        assert_eq!(
            document.validators.last_modified.as_deref(),
            Some("yesterday")
        );
        assert_eq!(document.moved_to, None);
        assert!(document.headers.contains_key("link"));
    }

    #[tokio::test]
    async fn sends_validators_and_understands_not_modified() {
        let server = MockServer::start().await;
        Mock::given(header("if-none-match", "\"abc\""))
            .and(header("if-modified-since", "yesterday"))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;
        let validators = Validators {
            etag: Some("\"abc\"".into()),
            last_modified: Some("yesterday".into()),
        };
        let outcome = client(true, "127.0.0.1")
            .fetch(
                &url(&server, "/doc"),
                &FetchOptions {
                    validators: Some(&validators),
                    ..options(100)
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::NotModified { moved_to: None }));
    }

    #[tokio::test]
    async fn the_size_limit_holds_with_and_without_a_length() {
        let server = MockServer::start().await;
        Mock::given(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(101)))
            .mount(&server)
            .await;
        let client = client(true, "127.0.0.1");
        let error = client
            .fetch(&url(&server, "/big"), &options(100))
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::TooLarge(100)), "{error}");
        assert!(
            client
                .fetch(&url(&server, "/big"), &options(101))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn redirects_are_followed_and_reported() {
        let server = MockServer::start().await;
        Mock::given(path("/moved"))
            .respond_with(ResponseTemplate::new(301).insert_header("location", "/new"))
            .mount(&server)
            .await;
        Mock::given(path("/temporary"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/new"))
            .mount(&server)
            .await;
        Mock::given(path("/new"))
            .respond_with(ResponseTemplate::new(200).set_body_string("here"))
            .mount(&server)
            .await;
        let client = client(true, "127.0.0.1");

        let document = fetched(
            client
                .fetch(&url(&server, "/moved"), &options(100))
                .await
                .unwrap(),
        );
        assert_eq!(document.final_url, url(&server, "/new"));
        assert_eq!(document.moved_to, Some(url(&server, "/new")));

        let document = fetched(
            client
                .fetch(&url(&server, "/temporary"), &options(100))
                .await
                .unwrap(),
        );
        assert_eq!(document.final_url, url(&server, "/new"));
        assert_eq!(
            document.moved_to, None,
            "only permanent hops count as a move"
        );
    }

    #[tokio::test]
    async fn redirect_loops_and_bad_targets_stop() {
        let server = MockServer::start().await;
        Mock::given(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
            .mount(&server)
            .await;
        Mock::given(path("/ftp"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "ftp://x/"))
            .mount(&server)
            .await;
        Mock::given(path("/nowhere"))
            .respond_with(ResponseTemplate::new(302))
            .mount(&server)
            .await;
        let client = client(true, "127.0.0.1");
        for (route, check) in [
            (
                "/loop",
                (|e| matches!(e, FetchError::TooManyRedirects)) as fn(&FetchError) -> bool,
            ),
            ("/ftp", |e| matches!(e, FetchError::InvalidUrl(_))),
            ("/nowhere", |e| matches!(e, FetchError::Network(_))),
        ] {
            let error = client
                .fetch(&url(&server, route), &options(100))
                .await
                .unwrap_err();
            assert!(check(&error), "{route}: {error}");
        }
    }

    #[tokio::test]
    async fn error_statuses_carry_retry_after() {
        let server = MockServer::start().await;
        Mock::given(path("/busy"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "120"))
            .mount(&server)
            .await;
        let error = client(true, "127.0.0.1")
            .fetch(&url(&server, "/busy"), &options(100))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                FetchError::Status {
                    status: 503,
                    retry_after: Some(120)
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn ip_literals_are_blocked_before_connecting() {
        let client = client(false, "93.184.216.34");
        for raw in [
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/",
            "http://[::1]/",
        ] {
            let error = client
                .fetch(&Url::parse(raw).unwrap(), &options(100))
                .await
                .unwrap_err();
            assert!(
                matches!(error, FetchError::Blocked(PolicyError::Blocked)),
                "{raw}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn names_resolving_to_private_addresses_are_blocked() {
        let server = MockServer::start().await;
        Mock::given(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let port = server.address().port();
        let target = Url::parse(&format!("http://rebinding.example:{port}/")).unwrap();

        let error = client(false, "127.0.0.1")
            .fetch(&target, &options(100))
            .await
            .unwrap_err();
        assert!(
            matches!(error, FetchError::Blocked(PolicyError::Blocked)),
            "{error}"
        );

        assert!(
            client(true, "127.0.0.1")
                .fetch(&target, &options(100))
                .await
                .is_ok(),
            "allow_private lets operators reach their own network"
        );
    }

    #[test]
    fn urls_are_normalized_and_validated() {
        assert_eq!(
            parse_http_url("example.com/feed.xml").unwrap().as_str(),
            "https://example.com/feed.xml"
        );
        assert_eq!(
            parse_http_url(" http://example.com/a#frag ")
                .unwrap()
                .as_str(),
            "http://example.com/a"
        );
        assert!(parse_http_url("ftp://example.com/").is_err());
        assert!(parse_http_url("file:///etc/passwd").is_err());
        assert!(parse_http_url("https://user:pw@example.com/").is_err());
    }

    #[test]
    fn retry_after_accepts_seconds_and_dates() {
        let now = UNIX_EPOCH + Duration::from_mins(24_090_208); // 21 Oct 2015 07:28:00 GMT
        assert_eq!(parse_retry_after(" 120 ", now), Some(120));
        assert_eq!(parse_retry_after("-5", now), Some(0));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 08:28:00 GMT", now),
            Some(3600)
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 06:00:00 GMT", now),
            Some(0)
        );
        assert_eq!(parse_retry_after("soon", now), None);
    }
}
