//! Sessions in the database, with a random token in the cookie and only its hash
//! stored.

use std::{fmt, sync::Arc};

use sqlx::{FromRow, Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};
use tower_cookies::{
    Cookie, Cookies,
    cookie::{SameSite, time::Duration},
};
use web_mev_core::{
    Clock, PasswordError, PasswordHasher, SystemClock, random_token, secure_eq, sha256_hex,
    token_hash,
};
use web_mev_sqlite::write_tx;

/// Random bytes in a session token.
const SESSION_TOKEN_BYTES: usize = 32;
/// Random bytes in a CSRF token.
const CSRF_TOKEN_BYTES: usize = 24;

/// How the app reads its `users` table. See the crate docs for an example.
pub trait SessionUser: for<'r> FromRow<'r, SqliteRow> + Send + Sync + Unpin + 'static {
    /// The session lookup, built with [`session_sql!`](crate::session_sql).
    const SESSION_SQL: &'static str;

    /// The password hash of an active account, given its id.
    const CREDENTIAL_SQL: &'static str;

    /// The id and password hash of an active account, given the trimmed login
    /// name. Add `COLLATE NOCASE` here if names are case-insensitive.
    const LOGIN_SQL: &'static str;

    /// How many accounts exist. First-run setup is closed once this isn't zero.
    const USER_COUNT_SQL: &'static str = "SELECT count(*) FROM users";
}

/// What happens when a browser route finds no session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginPolicy {
    /// GET and HEAD redirect to this path, and htmx requests navigate there.
    /// Other requests get the 401 page.
    Redirect(String),
    /// Every request gets the 401 page.
    Status,
}

/// Cookie names and session rules.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Session cookie name.
    pub session_cookie: String,
    /// Cookie carrying the CSRF token of anonymous forms.
    pub anon_csrf_cookie: String,
    /// Mark cookies `Secure`. Turn this on whenever the app is served over HTTPS.
    pub secure: bool,
    /// How long a session lasts, in seconds.
    pub session_ttl_secs: i64,
    /// What happens when a browser route finds no session.
    pub login_policy: LoginPolicy,
    /// Failed logins allowed before an account is locked.
    pub failure_limit: i64,
    /// How long an account stays locked, in seconds.
    pub lock_secs: i64,
}

impl AuthConfig {
    /// Cookies named `{prefix}_session` and `{prefix}_anon_csrf`, 30-day sessions,
    /// redirects to `/login`, and a 15-minute lock after 5 failures.
    #[must_use]
    pub fn new(cookie_prefix: &str, secure: bool) -> Self {
        Self {
            session_cookie: format!("{cookie_prefix}_session"),
            anon_csrf_cookie: format!("{cookie_prefix}_anon_csrf"),
            secure,
            session_ttl_secs: 30 * 86_400,
            login_policy: LoginPolicy::Redirect("/login".into()),
            failure_limit: 5,
            lock_secs: 900,
        }
    }
}

/// Why an authentication step failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Wrong login name or password. Which one is deliberately not said.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Too many failed logins for this name. Tell the user to wait.
    #[error("too many failed logins")]
    Locked,
    /// The password changed, or the account was disabled, after it was checked.
    #[error("credentials changed")]
    CredentialsChanged,
    /// First-run setup has already happened.
    #[error("already set up")]
    AlreadyBootstrapped,
    /// A new password isn't acceptable. The message is safe to display.
    #[error(transparent)]
    Password(#[from] PasswordError),
    /// A database error.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// A signed-in session.
#[derive(Debug, Clone)]
pub struct Session<U> {
    /// Row id, which `SessionStore::revoke` takes.
    pub id: i64,
    /// The account's id.
    pub user_id: i64,
    /// This session's CSRF token, for forms and the `csrf-token` meta tag.
    pub csrf_token: String,
    /// The app's user.
    pub user: U,
}

impl<U> Session<U> {
    /// Checks a submitted CSRF token. `CsrfForm` already does this. Call it only
    /// for bodies that extractor can't read, such as a streamed upload.
    #[must_use]
    pub fn check_csrf(&self, supplied: &str) -> bool {
        secure_eq(&self.csrf_token, supplied)
    }
}

/// A password check that succeeded. Pass the hash on to
/// [`SessionStore::begin_verified`].
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// The account's id.
    pub user_id: i64,
    /// The hash that was verified.
    pub password_hash: String,
}

/// A session row that was inserted. The cookie only goes out through
/// [`set_cookie`](Self::set_cookie), so call that after the commit.
#[must_use = "the browser won't be signed in until set_cookie runs"]
#[derive(Debug)]
pub struct IssuedSession {
    cookie: Cookie<'static>,
}

impl IssuedSession {
    /// Signs the browser in.
    pub fn set_cookie(self, cookies: &Cookies) {
        cookies.add(self.cookie);
    }
}

/// Sessions, throttling and bootstrap. Clones share the pool, config, clock and
/// hasher.
#[derive(Clone)]
pub struct SessionStore {
    db: SqlitePool,
    config: Arc<AuthConfig>,
    clock: Arc<dyn Clock>,
    hasher: PasswordHasher,
}

impl fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionStore")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The throttle key: the login name, normalized and hashed.
fn identity_hash(login: &str) -> String {
    sha256_hex(login.trim().to_lowercase().as_bytes())
}

impl SessionStore {
    /// A store on `db`, using the system clock and the default hasher.
    #[must_use]
    pub fn new(db: SqlitePool, config: AuthConfig) -> Self {
        Self {
            db,
            config: Arc::new(config),
            clock: Arc::new(SystemClock),
            hasher: PasswordHasher::default(),
        }
    }

    /// Uses another clock, such as `FixedClock` in tests.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Clock) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Uses another hasher, such as one with different password length limits.
    #[must_use]
    pub fn with_hasher(mut self, hasher: PasswordHasher) -> Self {
        self.hasher = hasher;
        self
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// The hasher, for new passwords.
    #[must_use]
    pub fn hasher(&self) -> &PasswordHasher {
        &self.hasher
    }

    /// The pool.
    #[must_use]
    pub fn db(&self) -> &SqlitePool {
        &self.db
    }

    /// Loads the session of a request's cookies, if it has a live one.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the query fails.
    pub async fn load<U: SessionUser>(
        &self,
        cookies: &Cookies,
    ) -> Result<Option<Session<U>>, AuthError> {
        let Some(cookie) = cookies.get(&self.config.session_cookie) else {
            return Ok(None);
        };
        let row = sqlx::query(U::SESSION_SQL)
            .bind(token_hash(cookie.value()))
            .bind(self.clock.unix())
            .fetch_optional(&self.db)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(Session {
            id: row.try_get("session_id")?,
            user_id: row.try_get("session_user_id")?,
            csrf_token: row.try_get("session_csrf_token")?,
            user: U::from_row(&row)?,
        }))
    }

    /// Checks a login: refuses a locked name, verifies the password, and updates
    /// the throttle. An unknown name does the same work as a wrong password, so
    /// timing doesn't reveal which accounts exist.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Locked`], [`AuthError::InvalidCredentials`], or an
    /// internal failure.
    pub async fn authenticate<U: SessionUser>(
        &self,
        login: &str,
        password: String,
    ) -> Result<Authenticated, AuthError> {
        if self.login_locked(login).await? {
            return Err(AuthError::Locked);
        }
        let row: Option<(i64, String)> = sqlx::query_as(U::LOGIN_SQL)
            .bind(login.trim())
            .fetch_optional(&self.db)
            .await?;
        let (user_id, hash) = row.unzip();
        let verified = self.hasher.verify_or_dummy(password, hash.clone()).await?;
        if let (true, Some(user_id), Some(password_hash)) = (verified, user_id, hash) {
            self.login_success(login).await?;
            Ok(Authenticated {
                user_id,
                password_hash,
            })
        } else {
            self.login_failure(login).await?;
            Err(AuthError::InvalidCredentials)
        }
    }

    /// Whether logins for this name are locked right now.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the query fails.
    pub async fn login_locked(&self, login: &str) -> Result<bool, AuthError> {
        let locked_until: Option<Option<i64>> =
            sqlx::query_scalar("SELECT locked_until FROM login_throttle WHERE identity_hash = ?")
                .bind(identity_hash(login))
                .fetch_optional(&self.db)
                .await?;
        Ok(locked_until
            .flatten()
            .is_some_and(|until| until > self.clock.unix()))
    }

    /// Records a failed login, and locks the name once it reaches the limit.
    /// Failures during a lock don't extend it, and the count restarts after one.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the query fails.
    pub async fn login_failure(&self, login: &str) -> Result<(), AuthError> {
        sqlx::query(
            "INSERT INTO login_throttle (identity_hash, failures, locked_until, updated_at)
             VALUES (?1, 1, CASE WHEN 1 >= ?3 THEN ?2 + ?4 END, ?2)
             ON CONFLICT(identity_hash) DO UPDATE SET
               failures = CASE
                 WHEN login_throttle.locked_until IS NOT NULL AND login_throttle.locked_until > ?2
                   THEN login_throttle.failures
                 WHEN login_throttle.locked_until IS NOT NULL
                   THEN 1
                 ELSE login_throttle.failures + 1 END,
               locked_until = CASE
                 WHEN login_throttle.locked_until IS NOT NULL AND login_throttle.locked_until > ?2
                   THEN login_throttle.locked_until
                 WHEN login_throttle.locked_until IS NOT NULL
                   THEN NULL
                 WHEN login_throttle.failures + 1 >= ?3
                   THEN ?2 + ?4
                 ELSE NULL END,
               updated_at = ?2",
        )
        .bind(identity_hash(login))
        .bind(self.clock.unix())
        .bind(self.config.failure_limit)
        .bind(self.config.lock_secs)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Clears the throttle after a successful login.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the query fails.
    pub async fn login_success(&self, login: &str) -> Result<(), AuthError> {
        sqlx::query("DELETE FROM login_throttle WHERE identity_hash = ?")
            .bind(identity_hash(login))
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Starts the transaction that creates a session after a password check. It
    /// takes the write lock, then checks the account still has `expected_hash`. A
    /// login racing a password change can't leave a session behind, because either
    /// this sees the new hash, or the change's revocation sees the new session.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialsChanged`] when the hash differs or the
    /// account is no longer active.
    pub async fn begin_verified<U: SessionUser>(
        &self,
        user_id: i64,
        expected_hash: &str,
    ) -> Result<Transaction<'static, Sqlite>, AuthError> {
        let mut tx = write_tx(&self.db).await?;
        let current: Option<String> = sqlx::query_scalar(U::CREDENTIAL_SQL)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
        if current.is_some_and(|current| secure_eq(&current, expected_hash)) {
            Ok(tx)
        } else {
            Err(AuthError::CredentialsChanged)
        }
    }

    /// Starts the transaction for first-run setup. It takes the bootstrap lock,
    /// then checks there are still no accounts, so of any concurrent attempts only
    /// the first can succeed. Create the first user in it, then commit.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AlreadyBootstrapped`] when an account exists.
    pub async fn begin_bootstrap<U: SessionUser>(
        &self,
    ) -> Result<Transaction<'static, Sqlite>, AuthError> {
        let mut tx = write_tx(&self.db).await?;
        sqlx::query(
            "INSERT INTO bootstrap_lock (id, touched_at) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET touched_at = ?1",
        )
        .bind(self.clock.unix())
        .execute(&mut *tx)
        .await?;
        let count: i64 = sqlx::query_scalar(U::USER_COUNT_SQL)
            .fetch_one(&mut *tx)
            .await?;
        if count == 0 {
            Ok(tx)
        } else {
            Err(AuthError::AlreadyBootstrapped)
        }
    }

    /// Whether first-run setup is still open.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the query fails.
    pub async fn needs_bootstrap<U: SessionUser>(&self) -> Result<bool, AuthError> {
        let count: i64 = sqlx::query_scalar(U::USER_COUNT_SQL)
            .fetch_one(&self.db)
            .await?;
        Ok(count == 0)
    }

    /// Inserts a session in `tx`. Send the cookie with
    /// [`IssuedSession::set_cookie`] once `tx` has committed.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the insert fails.
    pub async fn insert_session(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        user_id: i64,
    ) -> Result<IssuedSession, AuthError> {
        let token = random_token(SESSION_TOKEN_BYTES);
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(token_hash(&token))
        .bind(user_id)
        .bind(random_token(CSRF_TOKEN_BYTES))
        .bind(self.clock.unix() + self.config.session_ttl_secs)
        .bind(self.clock.unix())
        .execute(&mut **tx)
        .await?;
        let cookie = Cookie::build((self.config.session_cookie.clone(), token))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(self.config.secure)
            .max_age(Duration::seconds(self.config.session_ttl_secs))
            .build();
        Ok(IssuedSession { cookie })
    }

    /// Ends the request's session and clears its cookie.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the delete fails.
    pub async fn destroy(&self, cookies: &Cookies) -> Result<(), AuthError> {
        if let Some(cookie) = cookies.get(&self.config.session_cookie) {
            sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
                .bind(token_hash(cookie.value()))
                .execute(&self.db)
                .await?;
        }
        cookies.remove(
            Cookie::build(self.config.session_cookie.clone())
                .path("/")
                .build(),
        );
        Ok(())
    }

    /// Ends one session of an account. The `user_id` condition stops one user
    /// revoking another's. Returns whether a session was removed.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the delete fails.
    pub async fn revoke<'e, E>(
        executor: E,
        user_id: i64,
        session_id: i64,
    ) -> Result<bool, AuthError>
    where
        E: sqlx::Executor<'e, Database = Sqlite>,
    {
        let result = sqlx::query("DELETE FROM sessions WHERE id = ? AND user_id = ?")
            .bind(session_id)
            .bind(user_id)
            .execute(executor)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Ends every session of an account but `keep`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the delete fails.
    pub async fn revoke_others<'e, E>(
        executor: E,
        user_id: i64,
        keep: i64,
    ) -> Result<u64, AuthError>
    where
        E: sqlx::Executor<'e, Database = Sqlite>,
    {
        let result = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id != ?")
            .bind(user_id)
            .bind(keep)
            .execute(executor)
            .await?;
        Ok(result.rows_affected())
    }

    /// Ends every session of an account. Run it in the same `write_tx` as a
    /// password change or account deactivation.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when the delete fails.
    pub async fn revoke_all<'e, E>(executor: E, user_id: i64) -> Result<u64, AuthError>
    where
        E: sqlx::Executor<'e, Database = Sqlite>,
    {
        let result = sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(executor)
            .await?;
        Ok(result.rows_affected())
    }

    /// Deletes expired sessions and throttle rows, for a periodic worker. Expired
    /// sessions already can't be used. This only reclaims space.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Database`] when a delete fails.
    pub async fn prune_expired(&self) -> Result<u64, AuthError> {
        let now = self.clock.unix();
        let sessions = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.db)
            .await?;
        let stale = now - self.config.lock_secs;
        let throttle = sqlx::query(
            "DELETE FROM login_throttle WHERE updated_at < ?
             AND (locked_until IS NULL OR locked_until <= ?)",
        )
        .bind(stale)
        .bind(now)
        .execute(&self.db)
        .await?;
        Ok(sessions.rows_affected() + throttle.rows_affected())
    }

    /// Issues the CSRF token of an anonymous form, such as login or bootstrap.
    /// Put the return value in the form's `csrf_token` field. `AnonForm` then
    /// checks that it matches this cookie.
    #[must_use]
    pub fn issue_anon_csrf(&self, cookies: &Cookies) -> String {
        let token = random_token(CSRF_TOKEN_BYTES);
        cookies.add(
            Cookie::build((self.config.anon_csrf_cookie.clone(), token.clone()))
                .path("/")
                .http_only(true)
                .same_site(SameSite::Strict)
                .secure(self.config.secure)
                .build(),
        );
        token
    }
}
