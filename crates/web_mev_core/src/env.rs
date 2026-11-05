//! Reading configuration from environment variables.
//!
//! Blank values count as unset, so `FOO=` in an env file or `${FOO:-}` in a
//! compose file means the same as leaving `FOO` out. Every error names the
//! full variable, prefix included.

use std::{
    collections::HashMap,
    fmt::{Debug, Display},
    net::IpAddr,
    ops::RangeInclusive,
    str::FromStr,
};

use url::Url;

/// A configuration value that can't be used.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    /// The value isn't valid Unicode.
    #[error("{name} is not valid Unicode")]
    NotUnicode {
        /// Full variable name.
        name: String,
    },
    /// The value doesn't parse, or is outside what the setting allows.
    #[error("invalid {name}: {reason}")]
    Invalid {
        /// Full variable name.
        name: String,
        /// What is wrong with it. Never includes the value of a secret.
        reason: String,
    },
    /// A required value is missing or blank.
    #[error("{name} is required")]
    Missing {
        /// Full variable name.
        name: String,
    },
}

enum Source {
    System,
    Map(HashMap<String, String>),
}

/// A source of settings with a fixed name prefix.
pub struct Env {
    prefix: String,
    source: Source,
}

impl Debug for Env {
    // A map may hold secrets, so only the prefix is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Env {
    /// Reads the process environment. Names passed to the other methods get
    /// `prefix` in front of them, so `Env::system("MY_APP_").var("PORT")`
    /// reads `MY_APP_PORT`.
    #[must_use]
    pub fn system(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            source: Source::System,
        }
    }

    /// Reads fixed values, for tests. Changing the real environment is
    /// `unsafe` and races with other tests.
    pub fn from_pairs<'a>(
        prefix: &str,
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        Self {
            prefix: prefix.to_owned(),
            source: Source::Map(
                pairs
                    .into_iter()
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
            ),
        }
    }

    fn full(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    fn invalid(&self, name: &str, reason: impl Display) -> EnvError {
        EnvError::Invalid {
            name: self.full(name),
            reason: reason.to_string(),
        }
    }

    /// The raw value, or `None` when it is unset or blank.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::NotUnicode`] when the value isn't valid Unicode.
    pub fn var(&self, name: &str) -> Result<Option<String>, EnvError> {
        let full = self.full(name);
        let value = match &self.source {
            Source::System => match std::env::var(&full) {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(EnvError::NotUnicode { name: full });
                }
            },
            Source::Map(map) => map.get(&full).cloned(),
        };
        Ok(value.filter(|value| !value.trim().is_empty()))
    }

    /// The value, which has to be set.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Missing`] when it is unset or blank.
    pub fn required(&self, name: &str) -> Result<String, EnvError> {
        self.var(name)?.ok_or_else(|| EnvError::Missing {
            name: self.full(name),
        })
    }

    /// The parsed value, or `None` when it is unset or blank.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] when the value doesn't parse.
    pub fn parsed<T>(&self, name: &str) -> Result<Option<T>, EnvError>
    where
        T: FromStr,
        T::Err: Display,
    {
        self.var(name)?
            .map(|raw| {
                raw.trim()
                    .parse()
                    .map_err(|error| self.invalid(name, error))
            })
            .transpose()
    }

    /// The parsed value, or `default` when it is unset or blank.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] when the value doesn't parse.
    pub fn parse<T>(&self, name: &str, default: T) -> Result<T, EnvError>
    where
        T: FromStr,
        T::Err: Display,
    {
        Ok(self.parsed(name)?.unwrap_or(default))
    }

    /// The parsed value or `default`, which has to fall inside `range`.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] when the value doesn't parse or is out of
    /// range.
    pub fn ranged<T>(&self, name: &str, default: T, range: RangeInclusive<T>) -> Result<T, EnvError>
    where
        T: FromStr + PartialOrd + Display,
        T::Err: Display,
    {
        let value = self.parse(name, default)?;
        if range.contains(&value) {
            Ok(value)
        } else {
            Err(self.invalid(
                name,
                format!("must be between {} and {}", range.start(), range.end()),
            ))
        }
    }

    /// A boolean: `1`, `true` or `yes`, and `0`, `false` or `no`.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] for any other value.
    pub fn bool(&self, name: &str, default: bool) -> Result<bool, EnvError> {
        match self.var(name)? {
            Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => Ok(true),
                "0" | "false" | "no" => Ok(false),
                _ => Err(self.invalid(name, "must be true or false")),
            },
            None => Ok(default),
        }
    }

    /// An absolute `http` or `https` URL.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] for anything else.
    pub fn http_url(&self, name: &str) -> Result<Option<Url>, EnvError> {
        let Some(raw) = self.var(name)? else {
            return Ok(None);
        };
        let url = Url::parse(raw.trim()).map_err(|error| self.invalid(name, error))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(self.invalid(name, "must be an absolute http(s) URL"));
        }
        Ok(Some(url))
    }

    /// A comma-separated list of IP addresses, such as trusted proxies.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] when an entry isn't an IP address.
    pub fn ip_list(&self, name: &str) -> Result<Vec<IpAddr>, EnvError> {
        self.var(name)?
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| {
                item.parse()
                    .map_err(|error| self.invalid(name, format!("entry {item}: {error}")))
            })
            .collect()
    }

    /// A comma-separated list of CORS origins: `*`, or values such as
    /// `https://app.example` with no path or trailing slash.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Invalid`] when an entry isn't an origin.
    pub fn origins(&self, name: &str) -> Result<Vec<String>, EnvError> {
        self.var(name)?
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|origin| {
                let valid = origin == "*"
                    || Url::parse(origin).is_ok_and(|url| {
                        matches!(url.scheme(), "http" | "https")
                            && url.host().is_some()
                            && url.path() == "/"
                            && !origin.ends_with('/')
                    });
                if valid {
                    Ok(origin.to_owned())
                } else {
                    Err(self.invalid(
                        name,
                        format!("entry {origin:?}: expected `*` or an origin such as https://app.example"),
                    ))
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Env, EnvError};

    fn env(pairs: &[(&'static str, &'static str)]) -> Env {
        Env::from_pairs("APP_", pairs.iter().copied())
    }

    #[test]
    fn blank_means_unset() {
        let env = env(&[("APP_KEY", ""), ("APP_SPACES", "   ")]);
        assert_eq!(env.var("KEY").unwrap(), None);
        assert_eq!(env.var("SPACES").unwrap(), None);
        assert_eq!(env.parse("KEY", 7_u32).unwrap(), 7);
        assert!(env.bool("KEY", true).unwrap());
        assert_eq!(env.http_url("KEY").unwrap(), None);
        assert!(matches!(env.required("KEY"), Err(EnvError::Missing { .. })));
    }

    #[test]
    fn errors_name_the_full_variable() {
        let env = env(&[("APP_PORT", "http")]);
        let message = env.parse("PORT", 1_u16).unwrap_err().to_string();
        assert!(message.starts_with("invalid APP_PORT:"), "{message}");
    }

    #[test]
    fn prefix_applies_to_lookups() {
        let env = env(&[("APP_PORT", "8080"), ("PORT", "1")]);
        assert_eq!(env.parse("PORT", 0_u16).unwrap(), 8080);
    }

    #[test]
    fn ranged_checks_defaults_and_values() {
        let env = env(&[("APP_DAYS", "400")]);
        assert!(env.ranged("DAYS", 30_u32, 1..=365).is_err());
        assert_eq!(env.ranged("OTHER", 30_u32, 1..=365).unwrap(), 30);
    }

    #[test]
    fn bool_accepts_the_documented_words() {
        for (raw, expected) in [
            ("1", true),
            ("YES", true),
            ("true", true),
            ("0", false),
            ("No", false),
        ] {
            let env = Env::from_pairs("", [("X", raw)]);
            assert_eq!(env.bool("X", !expected).unwrap(), expected, "{raw}");
        }
        assert!(
            Env::from_pairs("", [("X", "maybe")])
                .bool("X", true)
                .is_err()
        );
    }

    #[test]
    fn http_url_rejects_other_schemes() {
        assert!(
            Env::from_pairs("", [("X", "ftp://host")])
                .http_url("X")
                .is_err()
        );
        assert!(
            Env::from_pairs("", [("X", "not a url")])
                .http_url("X")
                .is_err()
        );
        let url = Env::from_pairs("", [("X", "https://host/base")])
            .http_url("X")
            .unwrap()
            .unwrap();
        assert_eq!(url.path(), "/base");
    }

    #[test]
    fn ip_list_skips_empty_entries() {
        let list = Env::from_pairs("", [("X", "10.0.0.1, ::1,")])
            .ip_list("X")
            .unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            Env::from_pairs("", [("X", "10.0.0.1,nope")])
                .ip_list("X")
                .is_err()
        );
        assert!(Env::from_pairs("", []).ip_list("X").unwrap().is_empty());
    }

    #[test]
    fn origins_are_bare_origins() {
        let list = Env::from_pairs("", [("X", "https://a.example, *")])
            .origins("X")
            .unwrap();
        assert_eq!(list, ["https://a.example", "*"]);
        assert!(
            Env::from_pairs("", [("X", "https://a.example/")])
                .origins("X")
                .is_err()
        );
        assert!(
            Env::from_pairs("", [("X", "https://a.example/path")])
                .origins("X")
                .is_err()
        );
        assert!(
            Env::from_pairs("", [("X", "a.example")])
                .origins("X")
                .is_err()
        );
    }
}
