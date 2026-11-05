//! Browser sessions, CSRF, login throttling, first-run bootstrap and audit, for
//! axum apps on SQLite.
//!
//! The crate owns the `sessions`, `login_throttle`, `bootstrap_lock` and
//! `audit_log` tables, whose SQL ships in [`schema::REFERENCE_SQL`] for the app to
//! copy into its migrations. The `users` table stays the app's, so the app says
//! how to read it through [`SessionUser`]:
//!
//! ```ignore
//! #[derive(Clone, sqlx::FromRow)]
//! pub struct User { pub id: i64, pub email: String }
//!
//! impl SessionUser for User {
//!     const SESSION_SQL: &'static str = session_sql!(
//!         columns: "u.id, u.email",
//!         joins: "",
//!         active: "u.deleted_at IS NULL"
//!     );
//!     const CREDENTIAL_SQL: &'static str =
//!         "SELECT password_hash FROM users WHERE id = ? AND deleted_at IS NULL";
//!     const LOGIN_SQL: &'static str =
//!         "SELECT id, password_hash FROM users WHERE email = ? AND deleted_at IS NULL";
//! }
//!
//! pub type UiSession = web_mev_auth::UiSession<User>;
//! pub type CsrfForm<T> = web_mev_auth::CsrfForm<User, T>;
//! ```
//!
//! A login then verifies the password, and creates the session in a transaction
//! that first checks the password hasn't changed since:
//!
//! ```ignore
//! let found = store.authenticate::<User>(&form.email, form.password).await?;
//! let mut tx = store.begin_verified::<User>(found.user_id, &found.password_hash).await?;
//! let issued = store.insert_session(&mut tx, found.user_id).await?;
//! audit::record(&mut *tx, Some(found.user_id), "user.login", None, None, &json!({})).await?;
//! tx.commit().await?;
//! issued.set_cookie(&cookies); // only after commit
//! ```
//!
//! The extractors reach the store through `FromRef`, and reject through the app's
//! `web_mev_axum::ErrorSurface`. They need `tower_cookies::CookieManagerLayer`.
//!
//! The login throttle is keyed by login name only, so someone who knows a name can
//! lock that account for a while. Keying by address as well would let an attacker
//! keep guessing from new addresses, so limit requests per address separately,
//! with `web_mev_axum::RateLimiter`.

pub mod audit;
pub mod extract;
pub mod schema;
pub mod store;

pub use extract::{
    AnonForm, ApiSession, CSRF_HEADER, CsrfForm, EmptyForm, OptionalSession, UiSession,
};
pub use schema::{
    ADOPT_AUDIT_COLUMNS_SQL, ADOPT_SESSION_IDS_SQL, REFERENCE_SQL, SchemaError, verify_schema,
};
pub use store::{
    AuthConfig, AuthError, Authenticated, IssuedSession, LoginPolicy, Session, SessionStore,
    SessionUser,
};

/// Builds [`SessionUser::SESSION_SQL`] from the app's parts, keeping the session
/// lookup and expiry filter here. All three parts are string literals: `columns`
/// lists what the user's `FromRow` reads from `users u`, `joins` holds any extra
/// `JOIN`s, and `active` is the condition for an account that may sign in.
#[macro_export]
macro_rules! session_sql {
    (columns: $columns:literal, joins: $joins:literal, active: $active:literal) => {
        concat!(
            "SELECT s.id AS session_id, s.user_id AS session_user_id, ",
            "s.csrf_token AS session_csrf_token, ",
            $columns,
            " FROM sessions s JOIN users u ON u.id = s.user_id ",
            $joins,
            " WHERE (",
            $active,
            ") AND s.token_hash = ? AND s.expires_at > ?"
        )
    };
}
