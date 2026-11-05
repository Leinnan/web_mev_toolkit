//! Running the server.

use std::{future::Future, io, net::SocketAddr};

use axum::Router;
use tokio::net::TcpListener;

/// Serves `app` until `shutdown` completes, then waits for requests in flight.
/// It provides `ConnectInfo<SocketAddr>`, which client address resolution reads.
///
/// # Errors
///
/// Returns the I/O error when the server fails.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
