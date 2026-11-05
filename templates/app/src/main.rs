//! The `{{project-name}}` server binary.

use web_mev_runtime::{LogConfig, init_logging, install_panic_hook, load_dotenv, report_fatal};

#[tokio::main]
async fn main() {
    install_panic_hook("{{project-name}}");
    // First, so RUST_LOG from an env file counts.
    load_dotenv();
    let logs = init_logging(&LogConfig::new("{{crate_name}}=info,tower_http=info"));
    let result = match logs {
        Ok(_guard) => {{crate_name}}::run().await,
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        report_fatal("{{project-name}}", &error);
        std::process::exit(1);
    }
}
