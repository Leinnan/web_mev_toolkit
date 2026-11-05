//! Sessions, CSRF, throttling and bootstrap through a real router, against user
//! tables shaped like the three apps this was extracted from.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::borrow::Cow;

use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{FromRow, Row, SqlitePool, migrate::Migrator, sqlite::SqliteRow};
use tower_cookies::{CookieManagerLayer, Cookies};
use web_mev_auth::{
    AnonForm, ApiSession, AuthConfig, AuthError, CsrfForm, LoginPolicy, OptionalSession,
    REFERENCE_SQL, SchemaError, SessionStore, SessionUser, UiSession, audit, session_sql,
    verify_schema,
};
use web_mev_axum::{
    ErrorSurface, RequestError, WebError, html_error_response, json_error_response,
};
use web_mev_sqlite::write_tx;
use web_mev_test::{Client, FixedClock, TestDb};

static DELETED_AT: Migrator = sqlx::migrate!("tests/schemas/deleted_at");
static IS_ACTIVE: Migrator = sqlx::migrate!("tests/schemas/is_active");
static AVATAR: Migrator = sqlx::migrate!("tests/schemas/avatar");

/// Shaped like klucznik.
#[derive(Debug, Clone, FromRow)]
struct SoftDeleted {
    #[allow(dead_code)]
    id: i64,
    #[allow(dead_code)]
    email: String,
}

impl SessionUser for SoftDeleted {
    const SESSION_SQL: &'static str = session_sql!(
        columns: "u.id, u.email",
        joins: "",
        active: "u.deleted_at IS NULL"
    );
    const CREDENTIAL_SQL: &'static str =
        "SELECT password_hash FROM users WHERE id = ? AND deleted_at IS NULL";
    const LOGIN_SQL: &'static str =
        "SELECT id, password_hash FROM users WHERE email = ? AND deleted_at IS NULL";
}

/// Shaped like `oxi_feed`.
#[derive(Debug, Clone, FromRow)]
struct Flagged {
    id: i64,
    username: String,
    is_admin: bool,
}

impl SessionUser for Flagged {
    const SESSION_SQL: &'static str = session_sql!(
        columns: "u.id, u.username, u.is_admin",
        joins: "",
        active: "u.is_active = 1"
    );
    const CREDENTIAL_SQL: &'static str =
        "SELECT password_hash FROM users WHERE id = ? AND is_active = 1";
    const LOGIN_SQL: &'static str =
        "SELECT id, password_hash FROM users WHERE username = ? AND is_active = 1";
}

/// Shaped like zbierak, with a hand-written `FromRow` over a joined table.
#[derive(Debug, Clone)]
struct WithAvatar {
    id: i64,
    avatar: Option<String>,
}

impl<'r> FromRow<'r, SqliteRow> for WithAvatar {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            avatar: row.try_get("avatar_sha256")?,
        })
    }
}

impl SessionUser for WithAvatar {
    const SESSION_SQL: &'static str = session_sql!(
        columns: "u.id, a.sha256 AS avatar_sha256",
        joins: "LEFT JOIN user_avatars a ON a.user_id = u.id",
        active: "1 = 1"
    );
    const CREDENTIAL_SQL: &'static str = "SELECT password_hash FROM users WHERE id = ?";
    const LOGIN_SQL: &'static str = "SELECT id, password_hash FROM users WHERE email = ?";
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("{0}")]
    Request(RequestError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

impl WebError for AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Request(error) => error.status,
            Self::Auth(AuthError::InvalidCredentials | AuthError::CredentialsChanged) => {
                StatusCode::UNAUTHORIZED
            }
            Self::Auth(AuthError::Locked) => StatusCode::TOO_MANY_REQUESTS,
            Self::Auth(AuthError::AlreadyBootstrapped) => StatusCode::NOT_FOUND,
            Self::Auth(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Request(error) => error.code,
            Self::Auth(_) => "auth",
        }
    }

    fn public_message(&self) -> Cow<'_, str> {
        Cow::Owned(self.to_string())
    }

    fn is_internal(&self) -> bool {
        false
    }
}

impl From<RequestError> for AppError {
    fn from(error: RequestError) -> Self {
        Self::Request(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        html_error_response(&self, |page| page.minimal_html("test"))
    }
}

struct ApiError(AppError);

impl From<RequestError> for ApiError {
    fn from(error: RequestError) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        json_error_response(&self.0)
    }
}

#[derive(Clone)]
struct AppState {
    store: SessionStore,
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

#[derive(Deserialize)]
struct LoginForm {
    login: String,
    password: String,
}

#[derive(Deserialize)]
struct NameForm {
    name: String,
}

async fn login_page(
    State(state): State<AppState>,
    cookies: Cookies,
) -> axum::response::Html<String> {
    let token = state.store.issue_anon_csrf(&cookies);
    axum::response::Html(format!(r#"<input name="csrf_token" value="{token}">"#))
}

async fn login<U: SessionUser>(
    State(state): State<AppState>,
    AnonForm { form, cookies }: AnonForm<LoginForm>,
) -> Result<&'static str, AppError> {
    let found = state
        .store
        .authenticate::<U>(&form.login, form.password)
        .await?;
    let mut tx = state
        .store
        .begin_verified::<U>(found.user_id, &found.password_hash)
        .await?;
    let issued = state.store.insert_session(&mut tx, found.user_id).await?;
    audit::record(
        &mut *tx,
        Some(found.user_id),
        "user.login",
        None,
        None,
        &json!({}),
    )
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    issued.set_cookie(&cookies);
    Ok("signed in")
}

async fn account<U: SessionUser>(ui: UiSession<U>) -> String {
    format!(
        r#"{} <meta name="csrf-token" content="{}">"#,
        ui.user_id(),
        ui.session.csrf_token
    )
}

async fn rename<U: SessionUser>(form: CsrfForm<U, NameForm>) -> String {
    form.form.name
}

async fn api_me<U: SessionUser>(session: ApiSession<U>) -> Json<i64> {
    Json(session.session.user_id)
}

async fn maybe<U: SessionUser>(session: OptionalSession<U>) -> &'static str {
    if session.session.is_some() {
        "in"
    } else {
        "out"
    }
}

async fn logout<U: SessionUser>(
    State(state): State<AppState>,
    form: CsrfForm<U, web_mev_auth::EmptyForm>,
) -> Result<&'static str, AppError> {
    state.store.destroy(&form.ui.cookies).await?;
    Ok("bye")
}

fn router<U: SessionUser>(store: SessionStore) -> Router {
    Router::new()
        .route("/login", get(login_page).post(login::<U>))
        .route("/account", get(account::<U>))
        .route("/rename", post(rename::<U>))
        .route("/api/me", get(api_me::<U>))
        .route("/maybe", get(maybe::<U>))
        .route("/logout", post(logout::<U>))
        .with_state(AppState { store })
        .layer(CookieManagerLayer::new())
}

const PASSWORD: &str = "correct horse battery";

struct Fixture {
    db: TestDb,
    clock: FixedClock,
    store: SessionStore,
    hash: String,
}

async fn fixture_with(migrator: &Migrator, config: AuthConfig) -> Fixture {
    let db = TestDb::new(migrator).await.unwrap();
    sqlx::raw_sql(REFERENCE_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    let clock = FixedClock::at(1_000_000);
    let store = SessionStore::new(db.pool.clone(), config).with_clock(clock.clone());
    let hash = store.hasher().hash(PASSWORD.to_owned()).await.unwrap();
    Fixture {
        db,
        clock,
        store,
        hash,
    }
}

async fn fixture(migrator: &Migrator) -> Fixture {
    fixture_with(migrator, AuthConfig::new("app", false)).await
}

async fn soft_deleted_fixture() -> Fixture {
    let fixture = fixture(&DELETED_AT).await;
    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash) VALUES (1, 'a@b.c', 'A', ?)",
    )
    .bind(&fixture.hash)
    .execute(&fixture.db.pool)
    .await
    .unwrap();
    fixture
}

async fn signed_in(login_name: &str, app: Router) -> Client {
    let mut client = Client::new(app);
    client.get("/login").await;
    let response = client
        .post_form("/login", &[("login", login_name), ("password", PASSWORD)])
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    client.get("/account").await;
    client
}

async fn session_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn reference_sql_satisfies_the_schema_check() {
    let fixture = fixture(&DELETED_AT).await;
    verify_schema(&fixture.db.pool).await.unwrap();
}

#[tokio::test]
async fn schema_check_lists_every_difference() {
    let db = TestDb::new(&DELETED_AT).await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE sessions (token_hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL);
         CREATE TABLE login_throttle (identity_hash TEXT PRIMARY KEY, failures TEXT,
            locked_until INTEGER, updated_at INTEGER NOT NULL);",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let Err(SchemaError::Mismatch(problems)) = verify_schema(&db.pool).await else {
        panic!("expected a mismatch");
    };
    let all = problems.join("\n");
    assert!(all.contains("sessions.id is missing"), "{all}");
    assert!(all.contains("sessions.csrf_token is missing"), "{all}");
    assert!(all.contains("sessions.token_hash"), "{all}");
    assert!(all.contains("login_throttle.failures is TEXT"), "{all}");
    assert!(all.contains("table bootstrap_lock is missing"), "{all}");
    assert!(all.contains("table audit_log is missing"), "{all}");
}

#[tokio::test]
async fn login_stores_only_the_hash_with_safe_cookie_attributes() {
    let fixture = fixture_with(&DELETED_AT, AuthConfig::new("app", true)).await;
    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash) VALUES (1, 'a@b.c', 'A', ?)",
    )
    .bind(&fixture.hash)
    .execute(&fixture.db.pool)
    .await
    .unwrap();
    let mut client = Client::new(router::<SoftDeleted>(fixture.store.clone()));
    client.get("/login").await;
    let response = client
        .post_form("/login", &[("login", "a@b.c"), ("password", PASSWORD)])
        .await;
    let set_cookie = response.header("set-cookie").unwrap().to_owned();
    assert!(set_cookie.starts_with("app_session="), "{set_cookie}");
    for attribute in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=2592000",
    ] {
        assert!(set_cookie.contains(attribute), "{attribute}: {set_cookie}");
    }
    let token = client.cookie("app_session").unwrap().to_owned();
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(&fixture.db.pool)
        .await
        .unwrap();
    assert_ne!(stored, token);
    assert_eq!(stored, web_mev_core::token_hash(&token));
}

#[tokio::test]
async fn soft_deleted_user_loses_access() {
    let fixture = soft_deleted_fixture().await;
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut client = signed_in("a@b.c", app.clone()).await;
    assert_eq!(client.get("/maybe").await.body, "in");
    sqlx::query("UPDATE users SET deleted_at = 1 WHERE id = 1")
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    assert_eq!(client.get("/maybe").await.body, "out");
    let mut other = Client::new(app);
    other.get("/login").await;
    let response = other
        .post_form("/login", &[("login", "a@b.c"), ("password", PASSWORD)])
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn flagged_user_shape_reads_its_columns() {
    let fixture = fixture(&IS_ACTIVE).await;
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, is_admin) VALUES (7, 'Ann', ?, 1)",
    )
    .bind(&fixture.hash)
    .execute(&fixture.db.pool)
    .await
    .unwrap();
    let app = router::<Flagged>(fixture.store.clone());
    let mut client = signed_in("ann", app).await;
    let response = client.get("/account").await;
    assert!(response.body.starts_with("7 "), "{}", response.body);

    let cookies = Cookies::default();
    cookies.add(tower_cookies::Cookie::new(
        "app_session",
        client.cookie("app_session").unwrap().to_owned(),
    ));
    let session = fixture
        .store
        .load::<Flagged>(&cookies)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.user.id, 7);
    assert_eq!(session.user.username, "Ann");
    assert!(session.user.is_admin);

    sqlx::query("UPDATE users SET is_active = 0")
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    assert_eq!(client.get("/maybe").await.body, "out");
}

#[tokio::test]
async fn joined_user_shape_reads_its_join() {
    let fixture = fixture(&AVATAR).await;
    sqlx::query("INSERT INTO users (id, email, password_hash) VALUES (3, 'a@b.c', ?)")
        .bind(&fixture.hash)
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    let app = router::<WithAvatar>(fixture.store.clone());
    let client = signed_in("a@b.c", app).await;
    let cookies = Cookies::default();
    cookies.add(tower_cookies::Cookie::new(
        "app_session",
        client.cookie("app_session").unwrap().to_owned(),
    ));
    let session = fixture
        .store
        .load::<WithAvatar>(&cookies)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((session.user.id, session.user.avatar), (3, None));

    sqlx::query("INSERT INTO user_avatars (user_id, sha256) VALUES (3, 'abc')")
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    let session = fixture
        .store
        .load::<WithAvatar>(&cookies)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.user.avatar.as_deref(), Some("abc"));
}

#[tokio::test]
async fn sessions_expire_with_the_clock() {
    let fixture = soft_deleted_fixture().await;
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut client = signed_in("a@b.c", app).await;
    fixture
        .clock
        .advance(fixture.store.config().session_ttl_secs - 1);
    assert_eq!(client.get("/maybe").await.body, "in");
    fixture.clock.advance(1);
    assert_eq!(client.get("/maybe").await.body, "out");
    assert_eq!(fixture.store.prune_expired().await.unwrap(), 1);
    assert_eq!(session_count(&fixture.db.pool).await, 0);
}

#[tokio::test]
async fn missing_session_follows_the_login_policy() {
    let fixture = soft_deleted_fixture().await;
    let mut client = Client::new(router::<SoftDeleted>(fixture.store.clone()));

    let response = client.get("/account").await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.location(), Some("/login"));

    let head = client
        .request(axum::http::Method::HEAD, "/account", &[], None)
        .await;
    assert_eq!(head.status, StatusCode::SEE_OTHER);

    let response = client.htmx_get("/account").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.header("hx-redirect"), Some("/login"));

    let response = client.post_form("/rename", &[("name", "x")]).await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );

    let response = client.get("/api/me").await;
    web_mev_test::contract::assert_json_error(&response, StatusCode::UNAUTHORIZED, "unauthorized");
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));
}

#[tokio::test]
async fn status_policy_never_redirects() {
    let config = AuthConfig {
        login_policy: LoginPolicy::Status,
        ..AuthConfig::new("app", false)
    };
    let fixture = fixture_with(&DELETED_AT, config).await;
    let mut client = Client::new(router::<SoftDeleted>(fixture.store.clone()));
    assert_eq!(
        client.get("/account").await.status,
        StatusCode::UNAUTHORIZED
    );
    let response = client.htmx_get("/account").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert!(response.header("hx-redirect").is_none());
}

#[tokio::test]
async fn state_changes_need_the_session_csrf_token() {
    let fixture = soft_deleted_fixture().await;
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut client = signed_in("a@b.c", app).await;

    let response = client.post_raw_form("/rename", &[("name", "x")]).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);

    let response = client
        .post_raw_form("/rename", &[("name", "x"), ("csrf_token", "forged")])
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);

    let response = client.post_form("/rename", &[("name", "field")]).await;
    assert_eq!(
        (response.status, response.body.as_str()),
        (StatusCode::OK, "field")
    );

    let response = client.htmx_post("/rename", &[("name", "header")]).await;
    assert_eq!(
        (response.status, response.body.as_str()),
        (StatusCode::OK, "header")
    );
}

#[tokio::test]
async fn csrf_token_of_another_session_is_refused() {
    let fixture = soft_deleted_fixture().await;
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut first = signed_in("a@b.c", app.clone()).await;
    let second = signed_in("a@b.c", app).await;
    assert_ne!(first.csrf, second.csrf);
    first.csrf.clone_from(&second.csrf);
    let response = first.post_form("/rename", &[("name", "x")]).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn anonymous_forms_need_the_double_submit_token() {
    let fixture = soft_deleted_fixture().await;
    let mut client = Client::new(router::<SoftDeleted>(fixture.store.clone()));
    let credentials = [("login", "a@b.c"), ("password", PASSWORD)];

    assert_eq!(
        client.post_raw_form("/login", &credentials).await.status,
        StatusCode::FORBIDDEN,
        "no cookie and no field"
    );
    client.set_cookie("app_anon_csrf", "");
    assert_eq!(
        client
            .post_raw_form(
                "/login",
                &[credentials[0], credentials[1], ("csrf_token", "")]
            )
            .await
            .status,
        StatusCode::FORBIDDEN,
        "an empty cookie must not match an empty field"
    );
    client.get("/login").await;
    assert_eq!(
        client
            .post_raw_form(
                "/login",
                &[credentials[0], credentials[1], ("csrf_token", "forged")]
            )
            .await
            .status,
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        client.post_form("/login", &credentials).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn failed_logins_lock_the_name_until_the_lock_expires() {
    let fixture = soft_deleted_fixture().await;
    let store = &fixture.store;
    for _ in 0..5 {
        let error = store
            .authenticate::<SoftDeleted>("a@b.c", "wrong password".into())
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials), "{error}");
    }
    let error = store
        .authenticate::<SoftDeleted>(" A@B.C ", PASSWORD.into())
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::Locked),
        "the right password stays locked out: {error}"
    );

    fixture.clock.advance(store.config().lock_secs - 1);
    assert!(store.login_locked("a@b.c").await.unwrap());
    store.login_failure("a@b.c").await.unwrap();
    fixture.clock.advance(1);
    assert!(
        !store.login_locked("a@b.c").await.unwrap(),
        "failures during a lock don't extend it"
    );

    let found = store
        .authenticate::<SoftDeleted>("a@b.c", PASSWORD.into())
        .await
        .unwrap();
    assert_eq!(found.user_id, 1);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM login_throttle")
        .fetch_one(&fixture.db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "success clears the throttle");
}

#[tokio::test]
async fn unknown_names_count_as_failures_too() {
    let fixture = soft_deleted_fixture().await;
    let store = &fixture.store;
    for _ in 0..5 {
        let error = store
            .authenticate::<SoftDeleted>("nobody@b.c", "whatever at all".into())
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials), "{error}");
    }
    assert!(store.login_locked("nobody@b.c").await.unwrap());
    let stored: String = sqlx::query_scalar("SELECT identity_hash FROM login_throttle")
        .fetch_one(&fixture.db.pool)
        .await
        .unwrap();
    assert!(
        !stored.contains("nobody"),
        "the raw name never rests in the table"
    );
}

#[tokio::test]
async fn a_limit_of_one_locks_on_the_first_failure() {
    let config = AuthConfig {
        failure_limit: 1,
        ..AuthConfig::new("app", false)
    };
    let fixture = fixture_with(&DELETED_AT, config).await;
    fixture.store.login_failure("a@b.c").await.unwrap();
    assert!(fixture.store.login_locked("a@b.c").await.unwrap());
}

#[tokio::test]
async fn a_password_change_stops_a_login_that_already_checked_the_old_one() {
    let fixture = soft_deleted_fixture().await;
    let store = &fixture.store;
    let found = store
        .authenticate::<SoftDeleted>("a@b.c", PASSWORD.into())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET password_hash = 'new' WHERE id = 1")
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    let error = store
        .begin_verified::<SoftDeleted>(found.user_id, &found.password_hash)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::CredentialsChanged), "{error}");
}

/// The klucznik regression: a login and a password change at the same time must
/// not leave a session that was created with the old password.
#[tokio::test]
async fn racing_login_and_password_change_leave_no_old_sessions() {
    for _ in 0..10 {
        let fixture = soft_deleted_fixture().await;
        let store = fixture.store.clone();
        let found = store
            .authenticate::<SoftDeleted>("a@b.c", PASSWORD.into())
            .await
            .unwrap();

        let login_store = store.clone();
        let login = tokio::spawn(async move {
            let Ok(mut tx) = login_store
                .begin_verified::<SoftDeleted>(found.user_id, &found.password_hash)
                .await
            else {
                return;
            };
            let issued = login_store
                .insert_session(&mut tx, found.user_id)
                .await
                .unwrap();
            drop(issued);
            tx.commit().await.unwrap();
        });
        let change_pool = fixture.db.pool.clone();
        let change = tokio::spawn(async move {
            let mut tx = write_tx(&change_pool).await.unwrap();
            sqlx::query("UPDATE users SET password_hash = 'new' WHERE id = 1")
                .execute(&mut *tx)
                .await
                .unwrap();
            SessionStore::revoke_all(&mut *tx, 1).await.unwrap();
            tx.commit().await.unwrap();
        });
        login.await.unwrap();
        change.await.unwrap();
        assert_eq!(session_count(&fixture.db.pool).await, 0);
    }
}

#[tokio::test]
async fn revocation_is_scoped_to_the_account() {
    let fixture = soft_deleted_fixture().await;
    let pool = &fixture.db.pool;
    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash) VALUES (2, 'x@y.z', 'X', 'h')",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut ids = Vec::new();
    for user in [1_i64, 1, 1, 2] {
        let mut tx = write_tx(pool).await.unwrap();
        drop(fixture.store.insert_session(&mut tx, user).await.unwrap());
        tx.commit().await.unwrap();
        ids.push(
            sqlx::query_scalar::<_, i64>("SELECT max(id) FROM sessions")
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    assert!(
        !SessionStore::revoke(pool, 1, ids[3]).await.unwrap(),
        "another user's session"
    );
    assert!(SessionStore::revoke(pool, 1, ids[0]).await.unwrap());
    assert_eq!(
        SessionStore::revoke_others(pool, 1, ids[1]).await.unwrap(),
        1
    );
    assert_eq!(session_count(pool).await, 2);
    assert_eq!(SessionStore::revoke_all(pool, 1).await.unwrap(), 1);
    assert_eq!(session_count(pool).await, 1, "user 2's session stays");
}

#[tokio::test]
async fn logout_needs_csrf_and_ends_the_session() {
    let fixture = soft_deleted_fixture().await;
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut client = signed_in("a@b.c", app).await;
    let forged = client.post_raw_form("/logout", &[]).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(client.get("/maybe").await.body, "in");
    let token = client.cookie("app_session").unwrap().to_owned();

    let response = client.post_form("/logout", &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(client.cookie("app_session").is_none());
    assert_eq!(session_count(&fixture.db.pool).await, 0);

    client.set_cookie("app_session", &token);
    assert_eq!(
        client.get("/maybe").await.body,
        "out",
        "the old token is dead"
    );
}

#[tokio::test]
async fn bootstrap_is_first_come_and_then_closed() {
    let fixture = fixture(&DELETED_AT).await;
    assert!(
        fixture
            .store
            .needs_bootstrap::<SoftDeleted>()
            .await
            .unwrap()
    );
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = fixture.store.clone();
        tasks.push(tokio::spawn(async move {
            let Ok(mut tx) = store.begin_bootstrap::<SoftDeleted>().await else {
                return 0;
            };
            sqlx::query(
                "INSERT INTO users (email, display_name, password_hash) VALUES (?, 'A', 'h')",
            )
            .bind(format!("user{index}@b.c"))
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            1
        }));
    }
    let mut created = 0;
    for task in tasks {
        created += task.await.unwrap();
    }
    assert_eq!(created, 1);
    assert!(
        !fixture
            .store
            .needs_bootstrap::<SoftDeleted>()
            .await
            .unwrap()
    );
    assert!(matches!(
        fixture.store.begin_bootstrap::<SoftDeleted>().await,
        Err(AuthError::AlreadyBootstrapped)
    ));
}

#[tokio::test]
async fn a_failed_audit_rolls_back_the_change_and_sends_no_cookie() {
    let fixture = soft_deleted_fixture().await;
    let pool = &fixture.db.pool;
    sqlx::query(
        "CREATE TRIGGER block_audit BEFORE INSERT ON audit_log
         BEGIN SELECT RAISE(ABORT, 'audit blocked'); END;",
    )
    .execute(pool)
    .await
    .unwrap();
    let app = router::<SoftDeleted>(fixture.store.clone());
    let mut client = Client::new(app);
    client.get("/login").await;
    let response = client
        .post_form("/login", &[("login", "a@b.c"), ("password", PASSWORD)])
        .await;
    assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(client.cookie("app_session").is_none());
    assert_eq!(session_count(pool).await, 0);
}

#[tokio::test]
async fn audit_records_in_the_callers_transaction() {
    let fixture = soft_deleted_fixture().await;
    let pool = &fixture.db.pool;
    let mut tx = write_tx(pool).await.unwrap();
    audit::record(
        &mut *tx,
        Some(1),
        "user.renamed",
        Some("user"),
        Some("1"),
        &json!({"a": 1}),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log")
            .fetch_one(pool)
            .await
            .unwrap()
    };
    assert_eq!(count().await, 0);
    audit::record(pool, None, "system.start", None, None, &json!({}))
        .await
        .unwrap();
    assert_eq!(count().await, 1);
}
