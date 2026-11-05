//! The app's error type, and its HTML and JSON surfaces.

use std::borrow::Cow;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use web_mev_auth::{AuthError, SchemaError};
use web_mev_axum::{
    ErrorPage, RequestError, WebError, escape_html, html_error_response, json_error_response,
};
use web_mev_sqlite::ConnectError;

use crate::i18n::I18nError;

/// Result of anything that can fail in the app.
pub type AppResult<T> = Result<T, AppError>;

/// Everything that can go wrong. Add your own variants here.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// No such page or record, or one the user may not know about.
    #[error("not found")]
    NotFound,
    /// A request the extractors rejected.
    #[error("{0}")]
    Request(RequestError),
    /// The database stayed locked past the busy timeout.
    #[error("the server is busy; retry shortly")]
    Busy,
    /// A bad setting, at startup.
    #[error(transparent)]
    Env(#[from] web_mev_core::EnvError),
    /// Logging couldn't start.
    #[error(transparent)]
    Logging(#[from] web_mev_runtime::LoggingError),
    /// The database couldn't be opened or migrated.
    #[error(transparent)]
    Connect(#[from] ConnectError),
    /// The database doesn't have the tables `web_mev_auth` needs.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// A translation table is broken.
    #[error(transparent)]
    I18n(#[from] I18nError),
    /// A query failed.
    #[error(transparent)]
    Database(sqlx::Error),
    /// An authentication step failed unexpectedly.
    #[error(transparent)]
    Auth(AuthError),
    /// Templates couldn't load.
    #[error(transparent)]
    Templates(#[from] web_mev_templates::TemplateError),
    /// A template couldn't render.
    #[error(transparent)]
    Render(#[from] tera::Error),
    /// The file system or network failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Security headers couldn't be built.
    #[error(transparent)]
    Header(#[from] axum::http::header::InvalidHeaderValue),
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        if web_mev_sqlite::is_busy(&error) {
            Self::Busy
        } else {
            Self::Database(error)
        }
    }
}

impl From<AuthError> for AppError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Database(error) => error.into(),
            other => Self::Auth(other),
        }
    }
}

impl From<RequestError> for AppError {
    fn from(error: RequestError) -> Self {
        Self::Request(error)
    }
}

impl WebError for AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Request(error) => error.status,
            Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Request(error) => error.code,
            Self::Busy => "busy",
            _ => "internal_error",
        }
    }

    fn public_message(&self) -> Cow<'_, str> {
        match self {
            Self::NotFound | Self::Request(_) | Self::Busy => Cow::Owned(self.to_string()),
            _ => Cow::Borrowed("internal server error"),
        }
    }

    fn is_internal(&self) -> bool {
        self.status().is_server_error() && !matches!(self, Self::Busy | Self::Request(_))
    }

    fn retry_after(&self) -> Option<u32> {
        matches!(self, Self::Busy).then_some(1)
    }
}

/// The error page. It doesn't use Tera, so it still works when a template is what
/// failed, and it links the stylesheet by its plain URL for the same reason.
fn error_page(page: &ErrorPage) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>{code} · {app}</title>
  <link rel="stylesheet" href="/static/app.css">
</head>
<body class="flex min-h-dvh items-center justify-center bg-surface text-ink antialiased">
  <main class="max-w-md p-6 text-center">
    <p class="text-sm text-muted">{code}</p>
    <h1 class="mt-1 text-2xl font-semibold">{heading}</h1>
    <p class="mt-2 text-muted">{message}</p>
    <a class="btn mt-6" href="/">{app}</a>
  </main>
</body>
</html>"#,
        code = page.status.as_u16(),
        app = escape_html(env!("CARGO_PKG_NAME")),
        heading = page.heading,
        message = page.message,
    )
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        html_error_response(&self, error_page)
    }
}

/// [`AppError`] rendered as JSON, for API routes.
#[derive(Debug)]
pub struct ApiError(pub AppError);

impl<E> From<E> for ApiError
where
    AppError: From<E>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        json_error_response(&self.0)
    }
}
