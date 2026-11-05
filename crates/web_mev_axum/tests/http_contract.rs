//! The whole layer wired the way an app wires it, with its own error type and a
//! concrete state.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unused_async_trait_impl
)]

use std::{borrow::Cow, net::IpAddr};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, FromRequestParts},
    http::{Method, StatusCode, request::Parts},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tower_cookies::{CookieManagerLayer, Cookies};
use web_mev_axum::{
    AccessLog, ApiBody, ApiJson, ApiPath, ApiQuery, ClientIp, Csp, ErrorSurface, Flash, FlashStore,
    HxTrigger, RateLimiter, RateLimits, RequestError, SecurityHeaders, TrustedProxies, UiForm,
    UiPath, WebError, access_log, enforce_rate_limit, health_routes, html_error_response,
    hx_redirect, is_htmx, json_error_response, security_headers,
};
use web_mev_test::{Client, contract};

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("authentication required")]
    Unauthorized,
    #[error("the server is busy")]
    Busy,
    #[error("secret database detail")]
    Database,
    #[error("{0}")]
    Request(RequestError),
}

impl WebError for AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Request(error) => error.status,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Busy => "busy",
            Self::Database => "internal_error",
            Self::Request(error) => error.code,
        }
    }

    fn public_message(&self) -> Cow<'_, str> {
        match self {
            Self::Database => Cow::Borrowed("internal server error"),
            other => Cow::Owned(other.to_string()),
        }
    }

    fn is_internal(&self) -> bool {
        matches!(self, Self::Database)
    }

    fn retry_after(&self) -> Option<u32> {
        matches!(self, Self::Busy).then_some(1)
    }
}

impl From<RequestError> for AppError {
    fn from(error: RequestError) -> Self {
        Self::Request(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        html_error_response(&self, |page| page.minimal_html("Test <app>"))
    }
}

struct ApiError(AppError);

impl From<RequestError> for ApiError {
    fn from(error: RequestError) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        json_error_response(&self.0)
    }
}

#[derive(Clone)]
struct AppState {
    proxies: TrustedProxies,
    flash: FlashStore,
}

impl ErrorSurface for AppState {
    type Html = AppError;
    type Json = ApiError;
}

impl FromRef<AppState> for TrustedProxies {
    fn from_ref(state: &AppState) -> Self {
        state.proxies.clone()
    }
}

const BODY_LIMIT: usize = 1024;

#[derive(Deserialize, serde::Serialize)]
struct Item {
    name: String,
}

#[derive(Deserialize)]
struct Page {
    #[allow(dead_code)]
    limit: u32,
}

/// Runs before any body extractor, like a real credential check.
struct Principal;

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if parts.headers.contains_key("authorization") {
            Ok(Self)
        } else {
            Err(ApiError(AppError::Unauthorized))
        }
    }
}

async fn create(_principal: Principal, ApiJson(item): ApiJson<Item>) -> Json<Item> {
    Json(item)
}

async fn raw(_principal: Principal, body: ApiBody<BODY_LIMIT>) -> String {
    body.bytes.len().to_string()
}

async fn by_id(ApiPath(id): ApiPath<u32>) -> String {
    id.to_string()
}

async fn list(ApiQuery(_page): ApiQuery<Page>) -> &'static str {
    "ok"
}

async fn busy() -> Result<(), ApiError> {
    Err(ApiError(AppError::Busy))
}

async fn broken() -> Result<(), ApiError> {
    Err(ApiError(AppError::Database))
}

async fn api_not_found() -> ApiError {
    ApiError(RequestError::not_found().into())
}

async fn api_wrong_method() -> ApiError {
    ApiError(RequestError::method_not_allowed().into())
}

async fn ui_by_id(UiPath(id): UiPath<u32>) -> String {
    id.to_string()
}

async fn ui_form(UiForm(item): UiForm<Item>) -> String {
    item.name
}

async fn ui_busy() -> Result<(), AppError> {
    Err(AppError::Busy)
}

async fn ui_broken() -> Result<(), AppError> {
    Err(AppError::Database)
}

async fn ui_reject() -> Result<(), AppError> {
    Err(RequestError::bad_request("<script>alert(1)</script>").into())
}

async fn who(ClientIp(ip): ClientIp) -> String {
    ip.to_string()
}

async fn set_flash(axum::extract::State(state): axum::extract::State<AppState>, cookies: Cookies) {
    state.flash.set(&cookies, &Flash::<()>::success("saved"));
}

async fn get_flash(
    axum::extract::State(state): axum::extract::State<AppState>,
    cookies: Cookies,
) -> String {
    state
        .flash
        .take::<()>(&cookies)
        .map_or_else(|| "none".to_owned(), |flash| flash.message)
}

async fn strict_page() -> impl IntoResponse {
    (
        [
            ("content-security-policy", "sandbox"),
            ("cache-control", "public, max-age=60"),
        ],
        "icon",
    )
}

async fn htmx_page(headers: axum::http::HeaderMap) -> Response {
    if is_htmx(&headers) {
        return hx_redirect("/login").unwrap();
    }
    let mut response = "page".into_response();
    HxTrigger::new().event("refresh").apply(&mut response);
    response
}

fn app_with(limits: RateLimits, proxies: TrustedProxies) -> Router {
    let state = AppState {
        proxies: proxies.clone(),
        flash: FlashStore::new("app_flash", false),
    };
    let api = Router::new()
        .route(
            "/items",
            post(create).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route("/raw", post(raw))
        .route("/items/{id}", get(by_id))
        .route("/list", get(list))
        .route("/busy", get(busy))
        .route("/broken", get(broken))
        .fallback(api_not_found)
        .method_not_allowed_fallback(api_wrong_method);
    Router::new()
        .nest("/api/v1", api)
        .route("/ui/{id}", get(ui_by_id))
        .route("/form", post(ui_form))
        .route("/busy", get(ui_busy))
        .route("/broken", get(ui_broken))
        .route("/reject", get(ui_reject))
        .route("/who", get(who))
        .route("/flash/set", post(set_flash))
        .route("/flash/get", get(get_flash))
        .route("/strict", get(strict_page))
        .route("/htmx", get(htmx_page))
        .merge(health_routes(|| async {
            Err::<(), _>(std::io::Error::other("secret readiness detail"))
        }))
        .with_state(state)
        .layer(CookieManagerLayer::new())
        .layer(from_fn_with_state(
            RateLimiter::new(limits, proxies.clone()),
            enforce_rate_limit,
        ))
        .layer(from_fn_with_state(
            AccessLog::new(proxies).redact("/flash/", 0),
            access_log,
        ))
        .layer(from_fn_with_state(
            SecurityHeaders::new(&Csp::strict()).unwrap().hsts(60),
            security_headers,
        ))
}

fn client() -> Client {
    Client::new(app_with(RateLimits::default(), TrustedProxies::default()))
}

const AUTH: [(&str, &str); 1] = [("authorization", "Bearer token")];

#[tokio::test]
async fn unknown_api_routes_answer_with_json() {
    contract::unknown_route_is_json_404(&mut client(), "/api/v1/nope").await;
}

#[tokio::test]
async fn wrong_method_answers_with_json() {
    contract::wrong_method_is_json_405(&mut client(), Method::DELETE, "/api/v1/list").await;
}

#[tokio::test]
async fn oversized_bodies_answer_with_json() {
    contract::oversized_body_is_json_413(&mut client(), "/api/v1/items", &AUTH, BODY_LIMIT).await;
    contract::oversized_body_is_json_413(&mut client(), "/api/v1/raw", &AUTH, BODY_LIMIT).await;
}

#[tokio::test]
async fn auth_runs_before_body_parsing() {
    contract::auth_runs_before_the_body(&mut client(), "/api/v1/items", BODY_LIMIT).await;
}

#[tokio::test]
async fn json_extractors_classify_rejections() {
    let mut client = client();
    let response = client
        .request(
            Method::POST,
            "/api/v1/items",
            &[("content-type", "application/json"), AUTH[0]],
            Some("{not json".into()),
        )
        .await;
    contract::assert_json_error(&response, StatusCode::BAD_REQUEST, "bad_request");

    let response = client
        .post_json("/api/v1/items", &AUTH, &serde_json::json!({"wrong": 1}))
        .await;
    contract::assert_json_error(
        &response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    for media_type in [None, Some("text/plain")] {
        let mut headers = vec![AUTH[0]];
        headers.extend(media_type.map(|value| ("content-type", value)));
        for path in ["/api/v1/items", "/api/v1/raw"] {
            let response = client
                .request(Method::POST, path, &headers, Some("{}".into()))
                .await;
            contract::assert_json_error(
                &response,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
            );
        }
    }

    let response = client
        .post_json("/api/v1/items", &AUTH, &Item { name: "ok".into() })
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["name"], "ok");
}

#[tokio::test]
async fn path_and_query_reject_as_json() {
    let mut client = client();
    let response = client.get("/api/v1/items/not-a-number").await;
    contract::assert_json_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    let response = client.get("/api/v1/list?limit=no").await;
    contract::assert_json_error(&response, StatusCode::BAD_REQUEST, "bad_request");
    assert_eq!(client.get("/api/v1/items/7").await.body, "7");
}

#[tokio::test]
async fn browser_extractors_reject_as_html() {
    let mut client = client();
    let response = client.get("/ui/not-a-number").await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
    assert!(response.body.contains("<h1>Bad Request</h1>"));

    let response = client.post_raw_form("/form", &[("wrong", "x")]).await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );

    let huge = "x".repeat(web_mev_axum::FORM_BODY_LIMIT + 1);
    let response = client
        .post_raw_form("/form", &[("name", huge.as_str())])
        .await;
    assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);

    let response = client.post_raw_form("/form", &[("name", "ok")]).await;
    assert_eq!(response.body, "ok");
}

#[tokio::test]
async fn html_errors_escape_the_message_and_app_name() {
    let response = client().get("/reject").await;
    assert!(
        !response.body.contains("<script>alert"),
        "{}",
        response.body
    );
    assert!(response.body.contains("&lt;script&gt;alert(1)"));
    assert!(response.body.contains("Test &lt;app&gt;"));
}

#[tokio::test]
async fn retry_after_is_on_both_surfaces() {
    let mut client = client();
    for path in ["/busy", "/api/v1/busy"] {
        let response = client.get(path).await;
        assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(response.header("retry-after"), Some("1"), "{path}");
    }
}

#[tokio::test]
async fn internal_errors_are_redacted_on_both_surfaces() {
    let mut client = client();
    for path in ["/broken", "/api/v1/broken"] {
        let response = client.get(path).await;
        assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert!(!response.body.contains("secret"), "{}", response.body);
        assert!(response.body.contains("internal server error"), "{path}");
    }
}

#[tokio::test]
async fn security_headers_cover_routes_and_the_fallback() {
    let mut client = client();
    for path in ["/who", "/this/route/does/not/exist", "/api/v1/nope"] {
        let response = client.get(path).await;
        assert!(
            response
                .header("content-security-policy")
                .unwrap()
                .starts_with("default-src 'self'"),
            "{path}"
        );
        assert_eq!(
            response.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
        assert_eq!(
            response.header("referrer-policy"),
            Some("same-origin"),
            "{path}"
        );
        assert!(response.header("permissions-policy").is_some(), "{path}");
        assert_eq!(response.header("cache-control"), Some("no-store"), "{path}");
        assert_eq!(
            response.header("strict-transport-security"),
            Some("max-age=60"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_handler_can_set_something_stricter() {
    let response = client().get("/strict").await;
    assert_eq!(response.header("content-security-policy"), Some("sandbox"));
    assert_eq!(response.header("cache-control"), Some("public, max-age=60"));
    assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn health_is_up_and_readiness_hides_the_failure() {
    let mut client = client();
    let health = client.get("/health").await;
    assert_eq!(
        (health.status, health.body.as_str()),
        (StatusCode::OK, "ok")
    );
    let ready = client.get("/ready").await;
    assert_eq!(ready.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!ready.body.contains("secret"));
}

#[tokio::test]
async fn forwarded_header_only_counts_from_a_trusted_peer() {
    let proxy: IpAddr = "10.0.0.1".parse().unwrap();
    let app = app_with(RateLimits::default(), TrustedProxies::new([proxy]));
    let forged = [("x-forwarded-for", "6.6.6.6, 198.51.100.7")];

    let mut direct = Client::new(app.clone()).from_peer("203.0.113.9:1000".parse().unwrap());
    let response = direct.request(Method::GET, "/who", &forged, None).await;
    assert_eq!(response.body, "203.0.113.9");

    let mut proxied = Client::new(app).from_peer("10.0.0.1:1000".parse().unwrap());
    let response = proxied.request(Method::GET, "/who", &forged, None).await;
    assert_eq!(response.body, "198.51.100.7");
}

#[tokio::test]
async fn rate_limit_refuses_with_retry_after_and_exempts_health() {
    let limits = RateLimits {
        source: 2,
        ..RateLimits::default()
    };
    let mut client = Client::new(app_with(limits, TrustedProxies::default()));
    assert_eq!(client.get("/who").await.status, StatusCode::OK);
    assert_eq!(client.get("/who").await.status, StatusCode::OK);
    let refused = client.get("/who").await;
    contract::assert_json_error(&refused, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    assert!(
        refused
            .header("retry-after")
            .unwrap()
            .parse::<u32>()
            .unwrap()
            > 0
    );
    assert_eq!(client.get("/health").await.status, StatusCode::OK);
}

#[tokio::test]
async fn credential_budget_counts_bearer_tokens_across_addresses() {
    let limits = RateLimits {
        credential: 1,
        ..RateLimits::default()
    };
    let app = app_with(limits, TrustedProxies::default());
    let path = "/api/v1/items/1";
    let mut first = Client::new(app.clone()).from_peer("192.0.2.1:1".parse().unwrap());
    let mut second = Client::new(app).from_peer("192.0.2.2:1".parse().unwrap());
    assert_eq!(
        first.request(Method::GET, path, &AUTH, None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        second.request(Method::GET, path, &AUTH, None).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let other = [("authorization", "Bearer other")];
    assert_eq!(
        second.request(Method::GET, path, &other, None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn flash_survives_one_redirect_only() {
    let mut client = client();
    client.post_raw_form("/flash/set", &[]).await;
    assert_eq!(client.get("/flash/get").await.body, "saved");
    assert_eq!(client.get("/flash/get").await.body, "none");
}

#[tokio::test]
async fn htmx_helpers_set_their_headers() {
    let mut client = client();
    let response = client.htmx_get("/htmx").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.header("hx-redirect"), Some("/login"));
    let response = client.get("/htmx").await;
    assert_eq!(response.header("hx-trigger"), Some(r#"{"refresh":true}"#));
}

#[tokio::test]
async fn serve_provides_the_peer_address_and_stops_gracefully() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(web_mev_axum::serve(
        listener,
        app_with(RateLimits::default(), TrustedProxies::default()),
        async {
            let _ = stopped.await;
        },
    ));

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut stream,
        b"GET /who HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut reply = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut reply)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&reply).ends_with("127.0.0.1"));

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
