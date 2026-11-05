//! How `web_mev_auth` reads the `users` table.

use serde::Serialize;
use sqlx::FromRow;
use web_mev_auth::{SessionUser, session_sql};

/// A signed-in account.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    /// Row id.
    pub id: i64,
    /// Login name.
    pub email: String,
    /// Name to show.
    pub display_name: String,
}

impl SessionUser for User {
    const SESSION_SQL: &'static str = session_sql!(
        columns: "u.id, u.email, u.display_name",
        joins: "",
        active: "u.is_active = 1"
    );
    const CREDENTIAL_SQL: &'static str =
        "SELECT password_hash FROM users WHERE id = ? AND is_active = 1";
    const LOGIN_SQL: &'static str =
        "SELECT id, password_hash FROM users WHERE email = ? AND is_active = 1";
}

/// A signed-in browser session.
pub type UiSession = web_mev_auth::UiSession<User>;
/// A signed-in session on a JSON route.
pub type ApiSession = web_mev_auth::ApiSession<User>;
/// A CSRF-checked form from a signed-in browser.
pub type CsrfForm<T> = web_mev_auth::CsrfForm<User, T>;
