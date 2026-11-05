//! Process plumbing shared by web server binaries.
//!
//! A typical `main`, with env files loaded first so `RUST_LOG` from one counts:
//!
//! ```ignore
//! web_mev_runtime::install_panic_hook("my_app");
//! web_mev_runtime::load_dotenv();
//! let _logs = web_mev_runtime::init_logging(&LogConfig::new("info,sqlx=warn"))?;
//! if let Err(error) = my_app::run().await {
//!     web_mev_runtime::report_fatal("my_app", &error);
//!     std::process::exit(1);
//! }
//! ```

pub mod fatal;
pub mod logging;
pub mod shutdown;
pub mod workers;

pub use fatal::{install_panic_hook, report_fatal};
pub use logging::{LogConfig, LogGuard, LoggingError, init_logging, load_dotenv};
pub use shutdown::ShutdownSignal;
pub use workers::{Workers, sleep_or_stop};
