//! Settings from the environment.

use std::{fmt, net::IpAddr, net::SocketAddr, path::PathBuf};

use web_mev_core::{Env, EnvError};

/// Everything configurable.
#[derive(Clone)]
pub struct Config {
    /// Address to listen on.
    pub listen_addr: SocketAddr,
    /// SQLite URL.
    pub database_url: String,
    /// Mark cookies `Secure`. Turn this on behind HTTPS.
    pub cookie_secure: bool,
    /// Browser session lifetime, in days.
    pub session_days: i64,
    /// Token for creating the first account. Generated at startup when unset.
    pub setup_token: Option<String>,
    /// Proxies allowed to set `X-Forwarded-For`.
    pub trusted_proxies: Vec<IpAddr>,
    /// Requests per minute from one client address.
    pub rate_limit_per_source: u32,
    /// Read templates from here instead of the binary, and reload them on change.
    pub template_dir: Option<PathBuf>,
    /// Read static files from here instead of the binary.
    pub static_dir: Option<PathBuf>,
}

impl fmt::Debug for Config {
    // Written by hand so a secret can't reach the logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen_addr", &self.listen_addr)
            .field("database_url", &self.database_url)
            .field("cookie_secure", &self.cookie_secure)
            .field("session_days", &self.session_days)
            .field(
                "setup_token",
                &self.setup_token.as_ref().map(|_| "<redacted>"),
            )
            .field("trusted_proxies", &self.trusted_proxies)
            .field("rate_limit_per_source", &self.rate_limit_per_source)
            .field("template_dir", &self.template_dir)
            .field("static_dir", &self.static_dir)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            database_url: "sqlite://data/{{project-name}}.db?mode=rwc".to_owned(),
            cookie_secure: false,
            session_days: 30,
            setup_token: None,
            trusted_proxies: Vec::new(),
            rate_limit_per_source: 600,
            template_dir: None,
            static_dir: None,
        }
    }
}

impl Config {
    /// Reads the settings. Anything unset keeps its default.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError`] naming the first setting that can't be used.
    pub fn from_env(env: &Env) -> Result<Self, EnvError> {
        let defaults = Self::default();
        Ok(Self {
            listen_addr: env.parse("LISTEN_ADDR", defaults.listen_addr)?,
            database_url: env.var("DATABASE_URL")?.unwrap_or(defaults.database_url),
            cookie_secure: env.bool("COOKIE_SECURE", defaults.cookie_secure)?,
            session_days: env.ranged("SESSION_DAYS", defaults.session_days, 1..=365)?,
            setup_token: env.var("SETUP_TOKEN")?,
            trusted_proxies: env.ip_list("TRUSTED_PROXIES")?,
            rate_limit_per_source: env.ranged(
                "RATE_LIMIT_PER_SOURCE",
                defaults.rate_limit_per_source,
                1..=u32::MAX,
            )?,
            template_dir: env.parsed("TEMPLATE_DIR")?,
            static_dir: env.parsed("STATIC_DIR")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use web_mev_core::Env;

    use super::Config;

    #[test]
    fn blank_values_keep_their_defaults() {
        let env = Env::from_pairs("X_", [("X_SETUP_TOKEN", ""), ("X_SESSION_DAYS", "")]);
        let config = Config::from_env(&env).unwrap();
        assert_eq!(config.setup_token, None);
        assert_eq!(config.session_days, 30);
    }

    #[test]
    fn invalid_values_are_errors() {
        let env = Env::from_pairs("X_", [("X_SESSION_DAYS", "9000")]);
        let error = Config::from_env(&env).unwrap_err().to_string();
        assert!(error.contains("X_SESSION_DAYS"), "{error}");
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let env = Env::from_pairs("X_", [("X_SETUP_TOKEN", "hunter2")]);
        let config = Config::from_env(&env).unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
    }
}
