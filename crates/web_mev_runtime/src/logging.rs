//! Tracing setup.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

/// Why logging couldn't start.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    /// A global subscriber is already installed.
    #[error("logging is already initialized: {0}")]
    AlreadyInitialized(String),
    /// The default filter doesn't parse.
    #[error("invalid default log filter {filter:?}: {reason}")]
    InvalidDefault {
        /// The filter passed in.
        filter: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The rolling log file couldn't be set up.
    #[error("could not set up log files: {0}")]
    File(String),
}

/// What [`init_logging`] sets up.
#[derive(Debug, Clone)]
pub struct LogConfig {
    default_filter: String,
    #[cfg(feature = "rolling-file")]
    file: Option<(std::path::PathBuf, String)>,
}

impl LogConfig {
    /// Logs to stdout, using `RUST_LOG` when it is set and `default_filter`
    /// otherwise. Use a filter such as `info,sqlx=warn`, not `my_app=info`: the
    /// access log and error reports of the `web_mev_*` crates have their own targets,
    /// and that would hide them.
    #[must_use]
    pub fn new(default_filter: &str) -> Self {
        Self {
            default_filter: default_filter.to_owned(),
            #[cfg(feature = "rolling-file")]
            file: None,
        }
    }

    /// Also writes to a file in `directory`, named `prefix` plus the date, and
    /// rolled over daily. Old files are kept.
    #[cfg(feature = "rolling-file")]
    #[must_use]
    pub fn rolling_file(mut self, directory: impl Into<std::path::PathBuf>, prefix: &str) -> Self {
        self.file = Some((directory.into(), prefix.to_owned()));
        self
    }

    fn filter(&self, rust_log: Option<&str>) -> Result<EnvFilter, LoggingError> {
        if let Some(raw) = rust_log {
            match EnvFilter::try_new(raw) {
                Ok(filter) => return Ok(filter),
                Err(error) => eprintln!(
                    "ignoring invalid RUST_LOG ({error}); using {}",
                    self.default_filter
                ),
            }
        }
        EnvFilter::try_new(&self.default_filter).map_err(|error| LoggingError::InvalidDefault {
            filter: self.default_filter.clone(),
            reason: error.to_string(),
        })
    }
}

/// Keeps file logging running: buffered lines are flushed when it drops, so
/// hold it until the end of `main`. Does nothing for stdout-only logging.
#[must_use = "dropping the guard stops file logging"]
#[derive(Debug)]
pub struct LogGuard {
    #[cfg(feature = "rolling-file")]
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Installs the global subscriber. Call [`load_dotenv`] first, or `RUST_LOG`
/// from an env file won't count.
///
/// # Errors
///
/// Returns [`LoggingError`] when a subscriber is already installed, the default
/// filter is invalid, or the log directory can't be used.
pub fn init_logging(config: &LogConfig) -> Result<LogGuard, LoggingError> {
    let filter = config.filter(std::env::var("RUST_LOG").ok().as_deref())?;
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer());

    #[cfg(feature = "rolling-file")]
    if let Some((directory, prefix)) = &config.file {
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(prefix)
            .filename_suffix("log")
            .build(directory)
            .map_err(|error| LoggingError::File(error.to_string()))?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            )
            .try_init()
            .map_err(|error| LoggingError::AlreadyInitialized(error.to_string()))?;
        return Ok(LogGuard { _file: Some(guard) });
    }

    registry
        .try_init()
        .map_err(|error| LoggingError::AlreadyInitialized(error.to_string()))?;
    Ok(LogGuard {
        #[cfg(feature = "rolling-file")]
        _file: None,
    })
}

/// Loads `.env` if there is one. A missing file is fine, and a malformed one is
/// reported on stderr, because logging hasn't started yet.
pub fn load_dotenv() {
    if let Err(error) = dotenvy::dotenv()
        && !error.not_found()
    {
        eprintln!("ignoring .env: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::LogConfig;

    #[test]
    fn valid_rust_log_wins() {
        let filter = LogConfig::new("app=info")
            .filter(Some("app=debug"))
            .unwrap();
        assert_eq!(filter.to_string(), "app=debug");
    }

    #[test]
    fn invalid_rust_log_falls_back() {
        let filter = LogConfig::new("app=info").filter(Some("a=b=c")).unwrap();
        assert_eq!(filter.to_string(), "app=info");
    }

    #[test]
    fn invalid_default_is_an_error() {
        assert!(LogConfig::new("a=b=c").filter(None).is_err());
    }
}
