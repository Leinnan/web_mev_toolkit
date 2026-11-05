//! Waiting for the process to be asked to stop.

use std::io;

/// Signal handlers installed ahead of time, so a failure to install them stops
/// startup instead of surfacing at shutdown, and a signal arriving during
/// startup isn't lost.
#[derive(Debug)]
pub struct ShutdownSignal {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}

impl ShutdownSignal {
    /// Installs the handlers. Needs a running tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns the OS error when a handler can't be installed.
    pub fn install() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                terminate: signal(SignalKind::terminate())?,
                interrupt: signal(SignalKind::interrupt())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Resolves on SIGTERM or Ctrl-C, naming the signal.
    pub async fn wait(mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.terminate.recv() => "SIGTERM",
                _ = self.interrupt.recv() => "SIGINT",
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            "Ctrl-C"
        }
    }
}
