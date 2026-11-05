//! The `{{project-name}}` web app.

pub mod config;
pub mod error;
pub mod i18n;
pub mod routes;
pub mod state;
pub mod user;

use std::time::Duration;

pub use config::Config;
pub use error::{AppError, AppResult};
pub use state::AppState;
use tokio::net::TcpListener;
use web_mev_core::Env;
use web_mev_runtime::{ShutdownSignal, Workers, sleep_or_stop};

/// Migrations, embedded in the binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// How long a shutdown waits for background work.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// How often expired sessions are deleted.
const PRUNE_INTERVAL: Duration = Duration::from_hours(1);

/// Runs the server until it's asked to stop.
///
/// # Errors
///
/// Returns an error when the configuration is invalid, the database can't be
/// opened or doesn't match, or the listener can't start.
pub async fn run() -> AppResult<()> {
    let config = Config::from_env(&Env::system("{{env_prefix}}_"))?;
    tracing::info!(
        ?config,
        "starting {{project-name}} {}",
        env!("CARGO_PKG_VERSION")
    );
    let state = AppState::open(config).await?;
    if state.store.needs_bootstrap::<user::User>().await? {
        // There is no account yet, so this is the only way to create one.
        tracing::warn!(setup_token = %state.setup_token, "no account exists: open /bootstrap");
    }

    #[cfg(feature = "reload")]
    let _template_watch = state
        .templates
        .watch()
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let mut workers = Workers::new();
    let store = state.store.clone();
    workers.spawn("prune", move |mut stop| async move {
        while !sleep_or_stop(&mut stop, PRUNE_INTERVAL).await {
            if let Err(error) = store.prune_expired().await {
                tracing::error!(%error, "pruning expired sessions failed");
            }
        }
    });

    let listener = TcpListener::bind(state.config.listen_addr).await?;
    tracing::info!(address = %state.config.listen_addr, "listening");
    let shutdown = ShutdownSignal::install()?;
    web_mev_axum::serve(listener, routes::router(state), async move {
        let signal = shutdown.wait().await;
        tracing::info!(signal, "shutting down");
    })
    .await?;
    workers.shutdown(SHUTDOWN_GRACE).await;
    Ok(())
}
