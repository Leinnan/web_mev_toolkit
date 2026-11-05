//! The router and its handlers.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, Uri},
    middleware::from_fn_with_state,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use tower_cookies::{CookieManagerLayer, Cookies};
use web_mev_assets::Assets;
use web_mev_auth::{AnonForm, AuthError, EmptyForm, OptionalSession, SessionStore, audit};
use web_mev_axum::{
    AccessLog, Flash, FlashKind, HxTrigger, RateLimiter, RateLimits, RequestError, access_log,
    enforce_rate_limit, health_routes, is_htmx, security_headers,
};
use web_mev_core::{PasswordError, required_text, secure_eq};
use web_mev_i18n::safe_return_path;
use web_mev_sqlite::write_tx;

use crate::{
    AppError, AppResult, AppState,
    error::ApiError,
    i18n::Locale,
    user::{ApiSession, CsrfForm, UiSession, User},
};

/// Builds the whole app. The layers go on the outermost router, so they also cover
/// the 404 fallback.
pub fn router(state: AppState) -> Router {
    let limits = RateLimits {
        source: state.config.rate_limit_per_source,
        ..RateLimits::default()
    };
    let limiter = RateLimiter::new(limits, state.proxies.clone());
    let log = AccessLog::new(state.proxies.clone());
    let db = state.db.clone();
    let api = Router::new()
        .route("/me", get(api_me))
        .fallback(api_not_found)
        .method_not_allowed_fallback(api_wrong_method);
    let security = state.security.clone();
    Router::new()
        .route("/", get(home))
        .route("/notes", post(add_note))
        .route("/bootstrap", get(bootstrap_page).post(bootstrap))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/settings", get(settings_page))
        .route("/settings/password", post(change_password))
        .route("/locale/{code}", get(set_locale))
        .route("/static/{*path}", get(static_file))
        .nest("/api/v1", api)
        .merge(health_routes(move || {
            let db = db.clone();
            async move { web_mev_sqlite::ready(&db).await }
        }))
        .fallback(not_found)
        .with_state(state)
        .layer(CookieManagerLayer::new())
        .layer(from_fn_with_state(limiter, enforce_rate_limit))
        .layer(from_fn_with_state(log, access_log))
        .layer(from_fn_with_state(security, security_headers))
}

async fn not_found() -> AppError {
    AppError::NotFound
}

async fn api_not_found() -> ApiError {
    ApiError(AppError::NotFound)
}

async fn api_wrong_method() -> ApiError {
    ApiError(RequestError::method_not_allowed().into())
}

async fn api_me(ApiSession { session, .. }: ApiSession) -> Json<User> {
    Json(session.user)
}

async fn static_file(
    State(assets): State<Assets>,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> AppResult<Response> {
    assets
        .respond(&path, uri.query(), &headers)
        .await
        .ok_or(AppError::NotFound)
}

#[derive(Debug, Clone, Serialize, FromRow)]
struct Note {
    id: i64,
    body: String,
    created_at: i64,
}

async fn notes_of(state: &AppState, user_id: i64) -> AppResult<Vec<Note>> {
    Ok(sqlx::query_as(
        "SELECT id, body, created_at FROM notes WHERE user_id = ? ORDER BY id DESC LIMIT 100",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?)
}

async fn home(
    State(state): State<AppState>,
    Locale(locale): Locale,
    ui: UiSession,
) -> AppResult<Html<String>> {
    let mut context = state.session_context(&locale, &ui.cookies, &ui.session);
    context.insert("notes", &notes_of(&state, ui.user_id()).await?);
    state.render("home.html", &context)
}

#[derive(Deserialize)]
struct NoteForm {
    body: String,
}

async fn add_note(
    State(state): State<AppState>,
    Locale(locale): Locale,
    headers: HeaderMap,
    CsrfForm { ui, form }: CsrfForm<NoteForm>,
) -> AppResult<Response> {
    let body = required_text("note", &form.body, 2_000)
        .map_err(|error| RequestError::bad_request(error.0))?;
    sqlx::query("INSERT INTO notes (user_id, body) VALUES (?, ?)")
        .bind(ui.user_id())
        .bind(body)
        .execute(&state.db)
        .await?;
    let message = state.i18n.text(&locale, "note_added", &[]);
    if !is_htmx(&headers) {
        state.flash_next(&ui.cookies, &Flash::success(message));
        return Ok(Redirect::to("/").into_response());
    }
    // htmx swaps the list in place and shows the message as a toast.
    let mut context = state.session_context(&locale, &ui.cookies, &ui.session);
    context.insert("notes", &notes_of(&state, ui.user_id()).await?);
    let mut response = state
        .render("fragments/notes.html", &context)?
        .into_response();
    HxTrigger::new()
        .event_with("toast", &Flash::<()>::new(FlashKind::Success, message))
        .apply(&mut response);
    Ok(response)
}

async fn bootstrap_page(
    State(state): State<AppState>,
    Locale(locale): Locale,
    cookies: Cookies,
) -> AppResult<Html<String>> {
    if !state.store.needs_bootstrap::<User>().await? {
        return Err(AppError::NotFound);
    }
    let mut context = state.page_context(&locale, &cookies);
    context.insert("csrf_token", &state.store.issue_anon_csrf(&cookies));
    state.render("bootstrap.html", &context)
}

#[derive(Deserialize)]
struct BootstrapForm {
    setup_token: String,
    email: String,
    display_name: String,
    password: String,
}

async fn bootstrap(
    State(state): State<AppState>,
    Locale(locale): Locale,
    AnonForm { form, cookies }: AnonForm<BootstrapForm>,
) -> AppResult<Response> {
    let refuse = |key: &str, args: &[(&str, &str)]| -> AppResult<Response> {
        let mut context = state.page_context(&locale, &cookies);
        context.insert("csrf_token", &state.store.issue_anon_csrf(&cookies));
        context.insert("error", &state.i18n.text(&locale, key, args));
        context.insert("email", &form.email);
        context.insert("display_name", &form.display_name);
        Ok((
            StatusCode::BAD_REQUEST,
            state.render("bootstrap.html", &context)?,
        )
            .into_response())
    };
    if !state.store.needs_bootstrap::<User>().await? {
        return Err(AppError::NotFound);
    }
    // Compare hashes, so the comparison takes the same time for any length.
    if !secure_eq(
        &web_mev_core::token_hash(&form.setup_token),
        &web_mev_core::token_hash(&state.setup_token),
    ) {
        return refuse("bootstrap_invalid_token", &[]);
    }
    let email = required_text("email", &form.email, 254)
        .map_err(|error| RequestError::bad_request(error.0))?;
    let display_name = required_text("name", &form.display_name, 100)
        .map_err(|error| RequestError::bad_request(error.0))?;
    let hash = match state.store.hasher().hash(form.password.clone()).await {
        Ok(hash) => hash,
        Err(PasswordError::Length { min, max }) => {
            let (min, max) = (min.to_string(), max.to_string());
            return refuse("password_length", &[("min", &min), ("max", &max)]);
        }
        Err(error) => return Err(AuthError::from(error).into()),
    };

    let mut tx = match state.store.begin_bootstrap::<User>().await {
        Ok(tx) => tx,
        Err(AuthError::AlreadyBootstrapped) => return Err(AppError::NotFound),
        Err(error) => return Err(error.into()),
    };
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, password_hash) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(email)
    .bind(display_name)
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    let issued = state.store.insert_session(&mut tx, user_id).await?;
    audit::record(
        &mut *tx,
        Some(user_id),
        "user.bootstrapped",
        Some("user"),
        Some(&user_id.to_string()),
        &json!({}),
    )
    .await?;
    tx.commit().await?;
    issued.set_cookie(&cookies);
    Ok(Redirect::to("/").into_response())
}

async fn login_page(
    State(state): State<AppState>,
    Locale(locale): Locale,
    OptionalSession { session, cookies }: OptionalSession<User>,
) -> AppResult<Response> {
    if state.store.needs_bootstrap::<User>().await? {
        return Ok(Redirect::to("/bootstrap").into_response());
    }
    if session.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let mut context = state.page_context(&locale, &cookies);
    context.insert("csrf_token", &state.store.issue_anon_csrf(&cookies));
    Ok(state.render("login.html", &context)?.into_response())
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Locale(locale): Locale,
    AnonForm { form, cookies }: AnonForm<LoginForm>,
) -> AppResult<Response> {
    let found = match state
        .store
        .authenticate::<User>(&form.email, form.password)
        .await
    {
        Ok(found) => found,
        Err(error @ (AuthError::InvalidCredentials | AuthError::Locked)) => {
            let (status, key) = if matches!(error, AuthError::Locked) {
                (StatusCode::TOO_MANY_REQUESTS, "login_locked")
            } else {
                (StatusCode::UNAUTHORIZED, "login_invalid")
            };
            let mut context = state.page_context(&locale, &cookies);
            context.insert("csrf_token", &state.store.issue_anon_csrf(&cookies));
            context.insert("error", &state.i18n.text(&locale, key, &[]));
            context.insert("email", &form.email);
            return Ok((status, state.render("login.html", &context)?).into_response());
        }
        Err(error) => return Err(error.into()),
    };
    let mut tx = state
        .store
        .begin_verified::<User>(found.user_id, &found.password_hash)
        .await?;
    let issued = state.store.insert_session(&mut tx, found.user_id).await?;
    audit::record(
        &mut *tx,
        Some(found.user_id),
        "user.login",
        Some("user"),
        Some(&found.user_id.to_string()),
        &json!({}),
    )
    .await?;
    tx.commit().await?;
    issued.set_cookie(&cookies);
    Ok(Redirect::to("/").into_response())
}

async fn logout(
    State(state): State<AppState>,
    CsrfForm { ui, .. }: CsrfForm<EmptyForm>,
) -> AppResult<Redirect> {
    state.store.destroy(&ui.cookies).await?;
    Ok(Redirect::to("/login"))
}

async fn settings_page(
    State(state): State<AppState>,
    Locale(locale): Locale,
    ui: UiSession,
) -> AppResult<Html<String>> {
    let context = state.session_context(&locale, &ui.cookies, &ui.session);
    state.render("settings.html", &context)
}

#[derive(Deserialize)]
struct PasswordForm {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    Locale(locale): Locale,
    CsrfForm { ui, form }: CsrfForm<PasswordForm>,
) -> AppResult<Response> {
    let user_id = ui.user_id();
    let refuse = |status: StatusCode, message: String| -> AppResult<Response> {
        let mut context = state.session_context(&locale, &ui.cookies, &ui.session);
        context.insert("error", &message);
        Ok((status, state.render("settings.html", &context)?).into_response())
    };
    let current: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    let hasher = state.store.hasher();
    let correct = hasher
        .verify(form.current_password, current.clone())
        .await
        .map_err(AuthError::from)?;
    if !correct {
        let message = state.i18n.text(&locale, "settings_wrong_password", &[]);
        return refuse(StatusCode::UNAUTHORIZED, message);
    }
    let new_hash = match hasher.hash(form.new_password).await {
        Ok(hash) => hash,
        Err(PasswordError::Length { min, max }) => {
            let (min, max) = (min.to_string(), max.to_string());
            let args = [("min", min.as_str()), ("max", max.as_str())];
            let message = state.i18n.text(&locale, "password_length", &args);
            return refuse(StatusCode::BAD_REQUEST, message);
        }
        Err(error) => return Err(AuthError::from(error).into()),
    };

    // All or nothing, and only if nobody changed the password since we read it. A
    // login racing this can't leave a session behind: see `begin_verified`.
    let mut tx = write_tx(&state.db).await?;
    let changed =
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ? AND password_hash = ?")
            .bind(new_hash)
            .bind(user_id)
            .bind(&current)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if changed != 1 {
        let message = state.i18n.text(&locale, "settings_wrong_password", &[]);
        return refuse(StatusCode::UNAUTHORIZED, message);
    }
    SessionStore::revoke_all(&mut *tx, user_id).await?;
    audit::record(
        &mut *tx,
        Some(user_id),
        "user.password_changed",
        Some("user"),
        Some(&user_id.to_string()),
        &json!({}),
    )
    .await?;
    tx.commit().await?;
    state.store.destroy(&ui.cookies).await?;
    let message = state.i18n.text(&locale, "password_changed", &[]);
    state.flash_next(&ui.cookies, &Flash::success(message));
    Ok(Redirect::to("/login").into_response())
}

#[derive(Deserialize)]
struct LocaleQuery {
    next: Option<String>,
}

async fn set_locale(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Query(query): Query<LocaleQuery>,
    cookies: Cookies,
) -> AppResult<Redirect> {
    if !state.i18n.set_cookie(&cookies, &code) {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to(&safe_return_path(query.next.as_deref())))
}
