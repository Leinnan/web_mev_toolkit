//! Translations: the request's language, and the `t` template function.

use std::{convert::Infallible, sync::Arc};

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use tera::{Kwargs, State, Tera, TeraResult};
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use web_mev_i18n::{Catalog, LocaleCode, TranslationError, TranslationsAsset, format_template};

/// The translation table, inside the binary.
const TABLE: &str = include_str!("../locales/i18n.csv");
/// One year, in seconds.
const COOKIE_MAX_AGE: i64 = 31_536_000;

/// A broken translation table.
#[derive(Debug, thiserror::Error)]
pub enum I18nError {
    /// The table doesn't parse.
    #[error(transparent)]
    Table(#[from] TranslationError),
    /// The table's first column names no language.
    #[error("the translation table has no languages")]
    NoLanguages,
}

/// The parsed table, and how a browser picks a language.
#[derive(Debug)]
pub struct I18n {
    table: TranslationsAsset,
    supported: Vec<LocaleCode>,
    default: LocaleCode,
    cookie: String,
    secure: bool,
}

impl I18n {
    /// Parses the table. Its first language column is the default.
    ///
    /// # Errors
    ///
    /// Returns [`I18nError`] when the table can't be used.
    pub fn load(cookie: &str, secure: bool) -> Result<Self, I18nError> {
        let table = TranslationsAsset::from_csv_str(TABLE)?;
        let supported: Vec<LocaleCode> = table.locales().cloned().collect();
        let default = supported.first().cloned().ok_or(I18nError::NoLanguages)?;
        Ok(Self {
            table,
            supported,
            default,
            cookie: cookie.to_owned(),
            secure,
        })
    }

    /// The languages in the table.
    #[must_use]
    pub fn supported(&self) -> &[LocaleCode] {
        &self.supported
    }

    /// The whole table.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.table
    }

    /// The text of `key`, filled with `args`. A missing key shows as itself.
    #[must_use]
    pub fn text(&self, locale: &LocaleCode, key: &str, args: &[(&str, &str)]) -> String {
        use web_mev_i18n::Localize as _;
        web_mev_i18n::Lang {
            catalog: &self.table,
            locale: locale.clone(),
        }
        .t_args(key, args)
    }

    /// Remembers a chosen language. It returns `false` for an unsupported one.
    #[must_use]
    pub fn set_cookie(&self, cookies: &Cookies, code: &str) -> bool {
        let Ok(locale) = LocaleCode::try_from(code) else {
            return false;
        };
        if !self.supported.contains(&locale) {
            return false;
        }
        cookies.add(
            Cookie::build((self.cookie.clone(), locale.code()))
                .path("/")
                .http_only(true)
                .same_site(SameSite::Lax)
                .secure(self.secure)
                .max_age(tower_cookies::cookie::time::Duration::seconds(
                    COOKIE_MAX_AGE,
                ))
                .build(),
        );
        true
    }
}

/// Registers `t(key="...")` for templates. It reads the `locale` variable, so a
/// page only serializes the one string it asks for, not the whole table on each
/// render. `n=` picks a plural form, and any other argument fills a `{name}` slot.
pub fn register_translations(tera: &mut Tera, i18n: Arc<I18n>) {
    tera.register_function(
        "t",
        move |kwargs: Kwargs, state: &State| -> TeraResult<String> {
            use web_mev_i18n::Localize as _;
            let key: &str = kwargs.must_get("key")?;
            let code: Option<String> = state.get("locale")?;
            let locale = code
                .as_deref()
                .and_then(|code| LocaleCode::try_from(code).ok())
                .unwrap_or_else(|| i18n.default.clone());
            let filled: Vec<(String, String)> = kwargs
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.as_str()?;
                    (name != "key" && name != "n").then(|| (name.to_owned(), value.to_string()))
                })
                .collect();
            let args: Vec<(&str, &str)> = filled
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let lang = web_mev_i18n::Lang {
                catalog: &i18n.table,
                locale,
            };
            Ok(match kwargs.get::<i64>("n")? {
                Some(n) => lang.t_n(key, n.unsigned_abs(), &args),
                None => format_template(&lang.t(key), &args),
            })
        },
    );
}

/// The request's language: its cookie, then `Accept-Language`, then the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale(pub LocaleCode);

impl<S> FromRequestParts<S> for Locale
where
    S: Send + Sync,
    Arc<I18n>: FromRef<S>,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let i18n = Arc::<I18n>::from_ref(state);
        let cookie = Cookies::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|cookies| {
                cookies
                    .get(&i18n.cookie)
                    .map(|cookie| cookie.value().to_owned())
            });
        let accept = parts
            .headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok());
        Ok(Self(web_mev_i18n::negotiate(
            cookie.as_deref(),
            accept,
            &i18n.supported,
            &i18n.default,
        )))
    }
}
