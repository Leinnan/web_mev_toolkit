//! The HTTP layer shared by axum apps. It knows nothing about Tera or sqlx, so an
//! API-only app can use it too.
//!
//! Each app keeps its own error type with its own variants. It implements
//! [`WebError`] for the classification, and [`ErrorSurface`] on its state to say
//! which types the extractors reject with:
//!
//! ```ignore
//! impl WebError for AppError { /* status, code, public_message, is_internal */ }
//! impl From<RequestError> for AppError { /* ... */ }
//! impl IntoResponse for AppError {
//!     fn into_response(self) -> Response {
//!         html_error_response(&self, |page| page.minimal_html("My app"))
//!     }
//! }
//!
//! pub struct ApiError(pub AppError);
//! impl IntoResponse for ApiError {
//!     fn into_response(self) -> Response { json_error_response(&self.0) }
//! }
//! impl From<RequestError> for ApiError { /* ... */ }
//!
//! impl ErrorSurface for AppState { type Html = AppError; type Json = ApiError; }
//! ```

pub mod client_ip;
pub mod error;
pub mod extract;
pub mod flash;
pub mod health;
pub mod htmx;
pub mod log;
pub mod rate_limit;
pub mod security;
pub mod serve;

pub use client_ip::{ClientIp, TrustedProxies};
pub use error::{
    ApiErrorBody, ErrorPage, ErrorSurface, RequestError, WebError, escape_html,
    html_error_response, json_error_response,
};
pub use extract::{
    ApiBody, ApiJson, ApiPath, ApiQuery, FORM_BODY_LIMIT, UiForm, UiPath, UiQuery, read_limited,
};
pub use flash::{Flash, FlashKind, FlashStore};
pub use health::health_routes;
pub use htmx::{HxTrigger, hx_redirect, is_htmx};
pub use log::{AccessLog, access_log};
pub use rate_limit::{RateLimiter, RateLimits, enforce_rate_limit};
pub use security::{Csp, SecurityHeaders, security_headers};
pub use serve::serve;
