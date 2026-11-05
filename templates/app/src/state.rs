//! What handlers share.

use std::sync::Arc;

use axum::{extract::FromRef, response::Html};
use include_dir::{Dir, include_dir};
use serde::Serialize;
use sqlx::SqlitePool;
use tera::{Context, Tera};
use tower_cookies::Cookies;
use web_mev_assets::Assets;
use web_mev_auth::{AuthConfig, Session, SessionStore, verify_schema};
use web_mev_axum::{Csp, ErrorSurface, Flash, FlashStore, SecurityHeaders, TrustedProxies};
use web_mev_core::random_token;
use web_mev_i18n::LocaleCode;
use web_mev_sqlite::{DbTuning, connect_with};
use web_mev_templates::{
    FilterOptions, TemplateSource, Templates, register_filters, register_static_url,
};

use crate::{
    AppError, AppResult, Config, MIGRATOR,
    error::ApiError,
    i18n::{I18n, register_translations},
    user::User,
};

/// Cookie names start with this.
const COOKIE_PREFIX: &str = "{{crate_name}}";

static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates");
static STATIC: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/static");

/// Templates the app can't run without. A missing one stops startup.
const REQUIRED_TEMPLATES: &[&str] = &[
    "base.html",
    "bootstrap.html",
    "home.html",
    "login.html",
    "settings.html",
    "fragments/notes.html",
];

/// A language on the language switch.
#[derive(Debug, Serialize)]
struct LanguageChoice {
    code: String,
    name: String,
}

/// What handlers share. Clones are cheap.
#[derive(Clone)]
pub struct AppState {
    /// The settings.
    pub config: Arc<Config>,
    /// The database.
    pub db: SqlitePool,
    /// Sessions, login throttling and bootstrap.
    pub store: SessionStore,
    /// Page templates.
    pub templates: Templates,
    /// Static files.
    pub assets: Assets,
    /// One-shot messages.
    pub flash: FlashStore,
    /// Translations.
    pub i18n: Arc<I18n>,
    /// Proxies allowed to set `X-Forwarded-For`.
    pub proxies: TrustedProxies,
    /// The token that creates the first account.
    pub setup_token: Arc<str>,
    /// Headers on every response. Open up the CSP here when a page needs it.
    pub security: SecurityHeaders,
}

impl ErrorSurface for AppState {
    type Html = AppError;
    type Json = ApiError;
}

impl FromRef<AppState> for SessionStore {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

impl FromRef<AppState> for Arc<I18n> {
    fn from_ref(state: &AppState) -> Self {
        state.i18n.clone()
    }
}

impl FromRef<AppState> for TrustedProxies {
    fn from_ref(state: &AppState) -> Self {
        state.proxies.clone()
    }
}

impl FromRef<AppState> for Assets {
    fn from_ref(state: &AppState) -> Self {
        state.assets.clone()
    }
}

impl AppState {
    /// Opens the database and loads everything from `config`.
    ///
    /// # Errors
    ///
    /// Returns an error when the database, templates or translations can't be used.
    pub async fn open(config: Config) -> AppResult<Self> {
        let db = connect_with(&config.database_url, &MIGRATOR, &DbTuning::default()).await?;
        verify_schema(&db).await?;
        Self::with_pool(config, db)
    }

    /// Builds the state on a pool that is already open and migrated.
    ///
    /// # Errors
    ///
    /// Returns an error when templates or translations can't be used.
    pub fn with_pool(config: Config, db: SqlitePool) -> AppResult<Self> {
        let auth = AuthConfig {
            session_ttl_secs: config.session_days * 86_400,
            ..AuthConfig::new(COOKIE_PREFIX, config.cookie_secure)
        };
        let i18n = Arc::new(I18n::load(
            &format!("{COOKIE_PREFIX}_locale"),
            config.cookie_secure,
        )?);
        let assets = match &config.static_dir {
            Some(directory) => Assets::directory(directory, "/static"),
            None => Assets::embedded(&STATIC, "/static"),
        };
        let source = match &config.template_dir {
            Some(directory) => TemplateSource::Directory(directory.clone()),
            None => TemplateSource::Embedded(&TEMPLATES),
        };
        let templates = load_templates(source, assets.clone(), i18n.clone())?;
        Ok(Self {
            store: SessionStore::new(db.clone(), auth),
            db,
            templates,
            assets,
            flash: FlashStore::new(format!("{COOKIE_PREFIX}_flash"), config.cookie_secure),
            i18n,
            proxies: TrustedProxies::new(config.trusted_proxies.iter().copied()),
            setup_token: config
                .setup_token
                .clone()
                .unwrap_or_else(|| random_token(24))
                .into(),
            security: SecurityHeaders::new(&Csp::strict())?,
            config: Arc::new(config),
        })
    }

    /// Renders a page.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Render`] when the template fails.
    pub fn render(&self, template: &str, context: &Context) -> AppResult<Html<String>> {
        Ok(Html(self.templates.render(template, context)?))
    }

    /// What every page needs: language, the language switch, and any pending flash.
    #[must_use]
    pub fn page_context(&self, locale: &LocaleCode, cookies: &Cookies) -> Context {
        let mut context = Context::new();
        context.insert("app_name", "{{project-name}}");
        context.insert("locale", &locale.code());
        let languages: Vec<LanguageChoice> = self
            .i18n
            .supported()
            .iter()
            .filter(|candidate| *candidate != locale)
            .map(|candidate| LanguageChoice {
                code: candidate.code(),
                name: self.i18n.text(candidate, "language_name", &[]),
            })
            .collect();
        context.insert("languages", &languages);
        if let Some(flash) = self.flash.take::<()>(cookies) {
            context.insert("flash", &flash);
        }
        context
    }

    /// [`page_context`](Self::page_context) for a signed-in page.
    #[must_use]
    pub fn session_context(
        &self,
        locale: &LocaleCode,
        cookies: &Cookies,
        session: &Session<User>,
    ) -> Context {
        let mut context = self.page_context(locale, cookies);
        context.insert("user", &session.user);
        context.insert("csrf_token", &session.csrf_token);
        context
    }

    /// Stores a message for the next page.
    pub fn flash_next(&self, cookies: &Cookies, flash: &Flash) {
        self.flash.set(cookies, flash);
    }
}

fn load_templates(source: TemplateSource, assets: Assets, i18n: Arc<I18n>) -> AppResult<Templates> {
    Ok(Templates::new(
        source,
        REQUIRED_TEMPLATES,
        move |tera: &mut Tera| {
            register_filters(tera, &FilterOptions::default());
            let assets = assets.clone();
            register_static_url(tera, move |name| assets.url(name));
            register_translations(tera, i18n.clone());
        },
    )?)
}
