//! The server binary.

use web_mev_runtime::{LogConfig, init_logging, install_panic_hook, load_dotenv, report_fatal};

const NAME: &str = env!("CARGO_PKG_NAME");
/// Not just this crate: the access log and error reports come from `web_mev_*`.
const LOG_FILTER: &str = "info,sqlx=warn";

#[tokio::main]
async fn main() {
    install_panic_hook(NAME);
    // First, so RUST_LOG from an env file counts.
    load_dotenv();
    let logs = init_logging(&LogConfig::new(LOG_FILTER));
    let result = match logs {
        Ok(_guard) => {{crate_name}}::run().await,
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        report_fatal(NAME, &error);
        std::process::exit(1);
    }
}
