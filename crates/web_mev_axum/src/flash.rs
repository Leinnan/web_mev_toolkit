//! One-shot messages carried across a POST, redirect and GET.
//!
//! The message travels in a short-lived unsigned cookie and is removed when the
//! next page reads it. A forged cookie can only show its own text to its own
//! browser, as long as templates escape the message. Anything in `extra` is just
//! as forgeable, so treat it as client input.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tower_cookies::{
    Cookie, Cookies,
    cookie::{SameSite, time::Duration},
};

/// Longest message accepted back from the cookie, in characters.
const MAX_MESSAGE: usize = 300;

/// How a message should look. An unknown value from a cookie reads as `Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlashKind {
    /// Something worked.
    Success,
    /// Worked, with a caveat.
    Warning,
    /// Something failed.
    Danger,
    /// Neutral notice.
    #[serde(other)]
    Info,
}

/// A message shown once on the next page. `X` holds app data, such as an undo
/// form, and defaults to nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flash<X = ()> {
    /// How it should look.
    pub kind: FlashKind,
    /// Display text.
    pub message: String,
    /// App data. Not trustworthy, because it comes back from a cookie.
    pub extra: Option<X>,
}

impl<X> Flash<X> {
    /// A message of the given kind.
    pub fn new(kind: FlashKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            extra: None,
        }
    }

    /// A success message.
    pub fn success(message: impl Into<String>) -> Self {
        Self::new(FlashKind::Success, message)
    }

    /// A neutral notice.
    pub fn info(message: impl Into<String>) -> Self {
        Self::new(FlashKind::Info, message)
    }

    /// A warning.
    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(FlashKind::Warning, message)
    }

    /// An error message.
    pub fn danger(message: impl Into<String>) -> Self {
        Self::new(FlashKind::Danger, message)
    }

    /// Attaches app data.
    #[must_use]
    pub fn with_extra(mut self, extra: X) -> Self {
        self.extra = Some(extra);
        self
    }
}

/// Reads and writes the flash cookie.
#[derive(Debug, Clone)]
pub struct FlashStore {
    name: String,
    secure: bool,
}

impl FlashStore {
    /// Uses a cookie called `name`. Set `secure` when the app is served over
    /// HTTPS.
    #[must_use]
    pub fn new(name: impl Into<String>, secure: bool) -> Self {
        Self {
            name: name.into(),
            secure,
        }
    }

    /// Stores `flash` for the next page this browser renders.
    pub fn set<X: Serialize>(&self, cookies: &Cookies, flash: &Flash<X>) {
        let Ok(json) = serde_json::to_vec(flash) else {
            return;
        };
        let cookie = Cookie::build((self.name.clone(), URL_SAFE_NO_PAD.encode(json)))
            .path("/")
            .http_only(true)
            .secure(self.secure)
            .same_site(SameSite::Lax)
            .max_age(Duration::seconds(60))
            .build();
        cookies.add(cookie);
    }

    /// Removes and returns the pending message. A malformed cookie counts as none,
    /// and an overlong message is cut short.
    #[must_use]
    pub fn take<X: DeserializeOwned>(&self, cookies: &Cookies) -> Option<Flash<X>> {
        let raw = cookies.get(&self.name)?.value().to_owned();
        cookies.remove(Cookie::build(self.name.clone()).path("/").build());
        let mut flash: Flash<X> = URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
        if let Some((end, _)) = flash.message.char_indices().nth(MAX_MESSAGE) {
            flash.message.truncate(end);
        }
        Some(flash)
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use tower_cookies::{Cookie, Cookies};

    use super::{Flash, FlashKind, FlashStore, MAX_MESSAGE};

    fn store() -> FlashStore {
        FlashStore::new("app_flash", true)
    }

    fn with_raw(raw: &str) -> Cookies {
        let cookies = Cookies::default();
        cookies.add(Cookie::new("app_flash", raw.to_owned()));
        cookies
    }

    #[test]
    fn round_trip_removes_the_message() {
        let cookies = Cookies::default();
        store().set(&cookies, &Flash::<()>::success("saved"));
        let flash: Flash = store().take(&cookies).unwrap();
        assert_eq!(flash.kind, FlashKind::Success);
        assert_eq!(flash.message, "saved");
        assert!(store().take::<()>(&cookies).is_none());
    }

    #[test]
    fn cookie_has_safe_attributes() {
        let cookies = Cookies::default();
        store().set(&cookies, &Flash::<()>::info("x"));
        let cookie = cookies.get("app_flash").unwrap();
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.path(), Some("/"));
    }

    #[test]
    fn extra_round_trips() {
        let cookies = Cookies::default();
        store().set(&cookies, &Flash::info("x").with_extra(7_u32));
        assert_eq!(store().take::<u32>(&cookies).unwrap().extra, Some(7));
    }

    #[test]
    fn forged_values_are_contained() {
        let encode = |json: &str| URL_SAFE_NO_PAD.encode(json);
        assert!(store().take::<()>(&with_raw("not base64!")).is_none());
        assert!(store().take::<()>(&with_raw(&encode("not json"))).is_none());

        let unknown = encode(r#"{"kind":"evil","message":"x","extra":null}"#);
        let flash: Flash = store().take(&with_raw(&unknown)).unwrap();
        assert_eq!(flash.kind, FlashKind::Info);

        let long = "ż".repeat(MAX_MESSAGE + 50);
        let raw = encode(&format!(
            r#"{{"kind":"info","message":"{long}","extra":null}}"#
        ));
        let flash: Flash = store().take(&with_raw(&raw)).unwrap();
        assert_eq!(flash.message.chars().count(), MAX_MESSAGE);
    }
}
