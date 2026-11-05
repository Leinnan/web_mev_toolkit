//! Liveness and readiness endpoints.

use std::{error::Error, future::Future};

use axum::{Router, http::StatusCode, routing::get};

/// `GET /health`, which only says the process is up, and `GET /ready`, which runs
/// `check` and answers 503 when it fails. Mount these outside any authentication
/// or rate limit. The failure is logged, not sent to the caller.
pub fn health_routes<S, F, Fut, E>(check: F) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<(), E>> + Send,
    E: Error,
{
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/ready",
            get(move || async move {
                match check().await {
                    Ok(()) => (StatusCode::OK, "ready"),
                    Err(error) => {
                        tracing::error!(%error, "readiness check failed");
                        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
                    }
                }
            }),
        )
}
