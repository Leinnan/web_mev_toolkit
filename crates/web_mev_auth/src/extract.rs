//! Extractors for signed-in requests and CSRF-checked forms. Sessions resolve from
//! the request's parts, so anonymous requests never pay for reading a body.

use axum::{
    extract::{FromRef, FromRequest, FromRequestParts, Request},
    http::{Method, request::Parts},
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, de::DeserializeOwned};
use tower_cookies::Cookies;
use web_mev_axum::{
    ErrorSurface, FORM_BODY_LIMIT, RequestError, hx_redirect, is_htmx, read_limited,
};
use web_mev_core::secure_eq;

use crate::store::{AuthError, LoginPolicy, Session, SessionStore, SessionUser};

/// Header htmx requests use to carry the session's CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";

fn internal(context: &str, error: &dyn std::error::Error) -> RequestError {
    tracing::error!(%error, "{context}");
    RequestError::internal()
}

async fn cookies_of<S: Send + Sync>(parts: &mut Parts, state: &S) -> Result<Cookies, RequestError> {
    Cookies::from_request_parts(parts, state)
        .await
        .map_err(|(_, message)| {
            tracing::error!("{message}: is CookieManagerLayer installed?");
            RequestError::internal()
        })
}

fn load_failed(error: &AuthError) -> RequestError {
    internal("session lookup failed", error)
}

/// A signed-in browser session. With no session, this answers according to
/// [`LoginPolicy`].
#[derive(Debug, Clone)]
pub struct UiSession<U> {
    /// The session.
    pub session: Session<U>,
    /// Cookies, for handlers that set a flash message or end the session.
    pub cookies: Cookies,
}

impl<U> UiSession<U> {
    /// The signed-in account's id.
    #[must_use]
    pub fn user_id(&self) -> i64 {
        self.session.user_id
    }
}

fn no_session_response<S: ErrorSurface>(parts: &Parts, policy: &LoginPolicy) -> Response {
    if let LoginPolicy::Redirect(path) = policy {
        if is_htmx(&parts.headers)
            && let Ok(response) = hx_redirect(path)
        {
            return response;
        }
        if parts.method == Method::GET || parts.method == Method::HEAD {
            return Redirect::to(path).into_response();
        }
    }
    S::Html::from(RequestError::unauthorized()).into_response()
}

impl<S, U> FromRequestParts<S> for UiSession<U>
where
    S: ErrorSurface + Send + Sync,
    SessionStore: FromRef<S>,
    U: SessionUser,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let html = |error: RequestError| S::Html::from(error).into_response();
        let store = SessionStore::from_ref(state);
        let cookies = cookies_of(parts, state).await.map_err(html)?;
        match store.load::<U>(&cookies).await {
            Ok(Some(session)) => Ok(Self { session, cookies }),
            Ok(None) => Err(no_session_response::<S>(
                parts,
                &store.config().login_policy,
            )),
            Err(error) => Err(html(load_failed(&error))),
        }
    }
}

/// The session if there is one, for pages that work either way.
#[derive(Debug, Clone)]
pub struct OptionalSession<U> {
    /// The session.
    pub session: Option<Session<U>>,
    /// Cookies.
    pub cookies: Cookies,
}

impl<S, U> FromRequestParts<S> for OptionalSession<U>
where
    S: ErrorSurface + Send + Sync,
    SessionStore: FromRef<S>,
    U: SessionUser,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let html = |error: RequestError| S::Html::from(error).into_response();
        let store = SessionStore::from_ref(state);
        let cookies = cookies_of(parts, state).await.map_err(html)?;
        let session = store
            .load::<U>(&cookies)
            .await
            .map_err(|error| html(load_failed(&error)))?;
        Ok(Self { session, cookies })
    }
}

/// A signed-in session on a JSON route, which never redirects: it answers 401.
#[derive(Debug, Clone)]
pub struct ApiSession<U> {
    /// The session.
    pub session: Session<U>,
    /// Cookies.
    pub cookies: Cookies,
}

impl<S, U> FromRequestParts<S> for ApiSession<U>
where
    S: ErrorSurface + Send + Sync,
    SessionStore: FromRef<S>,
    U: SessionUser,
{
    type Rejection = S::Json;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let store = SessionStore::from_ref(state);
        let cookies = cookies_of(parts, state).await?;
        match store.load::<U>(&cookies).await {
            Ok(Some(session)) => Ok(Self { session, cookies }),
            Ok(None) => Err(RequestError::unauthorized().into()),
            Err(error) => Err(load_failed(&error).into()),
        }
    }
}

#[derive(Deserialize)]
struct CsrfField {
    #[serde(default)]
    csrf_token: Option<String>,
}

fn parse_form<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, RequestError> {
    serde_urlencoded::from_bytes(bytes)
        .map_err(|error| RequestError::bad_request(format!("invalid form: {error}")))
}

/// A state-changing browser request: a session plus a urlencoded body, accepted
/// only when the session's CSRF token arrives in the `X-CSRF-Token` header (htmx)
/// or a `csrf_token` field. A handler can't forget this check, because it can't
/// reach the form without it.
///
/// The body type doesn't need to name `csrf_token`. For a form with no other
/// fields, use [`EmptyForm`].
#[derive(Debug, Clone)]
pub struct CsrfForm<U, T, const LIMIT: usize = FORM_BODY_LIMIT> {
    /// The session.
    pub ui: UiSession<U>,
    /// The decoded body.
    pub form: T,
}

impl<S, U, T, const LIMIT: usize> FromRequest<S> for CsrfForm<U, T, LIMIT>
where
    S: ErrorSurface + Send + Sync,
    SessionStore: FromRef<S>,
    U: SessionUser,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let html = |error: RequestError| S::Html::from(error).into_response();
        let (mut parts, body) = request.into_parts();
        let ui = UiSession::<U>::from_request_parts(&mut parts, state).await?;
        let header_token = parts
            .headers
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = read_limited(Request::from_parts(parts, body), state, LIMIT)
            .await
            .map_err(html)?;
        let field: CsrfField = parse_form(&bytes).map_err(html)?;
        let supplied = header_token.or(field.csrf_token).unwrap_or_default();
        if !ui.session.check_csrf(&supplied) {
            return Err(html(RequestError::forbidden("invalid CSRF token")));
        }
        let form = parse_form(&bytes).map_err(html)?;
        Ok(Self { ui, form })
    }
}

/// A form for `CsrfForm` that has no fields of its own.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct EmptyForm {}

/// An anonymous form, such as login or bootstrap, checked by double submit: its
/// `csrf_token` field must match the cookie from
/// [`SessionStore::issue_anon_csrf`]. A cross-site page can't read that cookie, so
/// it can't fill in the field.
#[derive(Debug, Clone)]
pub struct AnonForm<T, const LIMIT: usize = FORM_BODY_LIMIT> {
    /// The decoded body.
    pub form: T,
    /// Cookies, for signing the browser in.
    pub cookies: Cookies,
}

impl<S, T, const LIMIT: usize> FromRequest<S> for AnonForm<T, LIMIT>
where
    S: ErrorSurface + Send + Sync,
    SessionStore: FromRef<S>,
    T: DeserializeOwned,
{
    type Rejection = S::Html;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let store = SessionStore::from_ref(state);
        let (mut parts, body) = request.into_parts();
        let cookies = cookies_of(&mut parts, state).await?;
        let expected = cookies
            .get(&store.config().anon_csrf_cookie)
            .map(|cookie| cookie.value().to_owned())
            .unwrap_or_default();
        let bytes = read_limited(Request::from_parts(parts, body), state, LIMIT).await?;
        let field: CsrfField = parse_form(&bytes)?;
        let supplied = field.csrf_token.unwrap_or_default();
        if expected.is_empty() || !secure_eq(&expected, &supplied) {
            return Err(RequestError::forbidden("invalid CSRF token").into());
        }
        Ok(Self {
            form: parse_form(&bytes)?,
            cookies,
        })
    }
}
