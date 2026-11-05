//! Extractors that reject with the app's own error types, so clients never get
//! axum's default plain-text rejection bodies.

use axum::{
    Json,
    body::Bytes,
    extract::{
        DefaultBodyLimit, FromRequest, FromRequestParts, Path, Query, Request,
        rejection::{BytesRejection, FailedToBufferBody, JsonRejection},
    },
    http::{HeaderMap, header::CONTENT_TYPE, request::Parts},
    response::IntoResponse,
};
use serde::de::DeserializeOwned;

use crate::error::{ErrorSurface, RequestError};

/// Default limit for urlencoded form bodies.
pub const FORM_BODY_LIMIT: usize = 64 * 1024;

fn body_rejection_error(rejection: &BytesRejection) -> RequestError {
    match rejection {
        BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_)) => {
            RequestError::payload_too_large()
        }
        _ => RequestError::bad_request("could not read request body"),
    }
}

fn json_rejection_error(rejection: &JsonRejection) -> RequestError {
    match rejection {
        JsonRejection::JsonSyntaxError(error) => RequestError::bad_request(error.body_text()),
        JsonRejection::JsonDataError(error) => RequestError::unprocessable(error.body_text()),
        JsonRejection::MissingJsonContentType(_) => RequestError::unsupported_media_type(),
        JsonRejection::BytesRejection(bytes) => body_rejection_error(bytes),
        _ => RequestError::bad_request("invalid JSON request body"),
    }
}

/// Accepts `application/json` and `+json` subtypes, with optional parameters
/// such as `; charset=utf-8`.
fn is_json_media_type(headers: &HeaderMap) -> bool {
    let Some(raw) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "application/json" || base.ends_with("+json")
}

/// Reads a whole body, enforcing `limit`.
///
/// # Errors
///
/// Returns a 413 [`RequestError`] over the limit, and a 400 when the body can't
/// be read.
pub async fn read_limited<S: Send + Sync>(
    mut request: Request,
    state: &S,
    limit: usize,
) -> Result<Bytes, RequestError> {
    DefaultBodyLimit::max(limit).apply(&mut request);
    Bytes::from_request(request, state)
        .await
        .map_err(|rejection| body_rejection_error(&rejection))
}

/// Defines a wrapper over `Path` or `Query` that rejects with a surface's type.
macro_rules! parts_extractor {
    ($(#[$doc:meta])* $name:ident, $inner:ident, $surface:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name<T>(pub T);

        impl<S, T> FromRequestParts<S> for $name<T>
        where
            S: ErrorSurface + Send + Sync,
            T: DeserializeOwned + Send,
        {
            type Rejection = S::$surface;

            async fn from_request_parts(
                parts: &mut Parts,
                state: &S,
            ) -> Result<Self, Self::Rejection> {
                match $inner::<T>::from_request_parts(parts, state).await {
                    Ok(value) => Ok(Self(value.0)),
                    Err(rejection) => {
                        Err(RequestError::bad_request(rejection.body_text()).into())
                    }
                }
            }
        }
    };
}

parts_extractor!(
    /// Path parameters on an API route.
    ApiPath, Path, Json
);
parts_extractor!(
    /// Query parameters on an API route.
    ApiQuery, Query, Json
);
parts_extractor!(
    /// Path parameters on a browser route.
    UiPath, Path, Html
);
parts_extractor!(
    /// Query parameters on a browser route.
    UiQuery, Query, Html
);

/// A JSON body on an API route. Set the size limit with a [`DefaultBodyLimit`]
/// layer.
#[derive(Debug, Clone, Copy)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: ErrorSurface + Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = S::Json;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection| json_rejection_error(&rejection).into())
    }
}

/// A raw JSON body on an API route, for handlers that need the exact bytes:
/// requires a JSON media type and enforces `LIMIT` bytes.
#[derive(Debug, Clone)]
pub struct ApiBody<const LIMIT: usize> {
    /// The buffered body.
    pub bytes: Bytes,
}

impl<S, const LIMIT: usize> FromRequest<S> for ApiBody<LIMIT>
where
    S: ErrorSurface + Send + Sync,
{
    type Rejection = S::Json;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_media_type(request.headers()) {
            return Err(RequestError::unsupported_media_type().into());
        }
        let bytes = read_limited(request, state, LIMIT).await?;
        Ok(Self { bytes })
    }
}

/// A urlencoded form on a browser route, with no CSRF check. Use it only for
/// routes that have no session yet. Everywhere else use `web_mev_auth::CsrfForm`.
#[derive(Debug, Clone, Copy)]
pub struct UiForm<T, const LIMIT: usize = FORM_BODY_LIMIT>(pub T);

impl<S, T, const LIMIT: usize> FromRequest<S> for UiForm<T, LIMIT>
where
    S: ErrorSurface + Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = S::Html;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = read_limited(request, state, LIMIT).await?;
        serde_urlencoded::from_bytes(&bytes)
            .map(Self)
            .map_err(|error| RequestError::bad_request(format!("invalid form: {error}")).into())
    }
}

impl IntoResponse for RequestError {
    /// Plain text, for code that has no surface. Apps should convert this into
    /// their own error type instead.
    fn into_response(self) -> axum::response::Response {
        (self.status, self.message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::CONTENT_TYPE};

    use super::is_json_media_type;

    #[test]
    fn json_media_types() {
        for (value, expected) in [
            ("application/json", true),
            ("application/json; charset=utf-8", true),
            ("APPLICATION/JSON", true),
            ("application/vnd.api+json", true),
            ("text/plain", false),
            ("application/xml", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(value));
            assert_eq!(is_json_media_type(&headers), expected, "{value}");
        }
        assert!(!is_json_media_type(&HeaderMap::new()));
    }
}
