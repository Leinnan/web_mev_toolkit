//! htmx request and response headers.

use std::fmt::Write as _;

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header::InvalidHeaderValue},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Whether the request came from htmx.
#[must_use]
pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

/// Answers an htmx request with a full-page navigation to `path`. The 401 keeps
/// htmx from swapping anything into the page.
///
/// # Errors
///
/// Returns an error when `path` holds characters a header can't.
pub fn hx_redirect(path: &str) -> Result<Response, InvalidHeaderValue> {
    let value = HeaderValue::from_str(path)?;
    Ok((StatusCode::UNAUTHORIZED, [("hx-redirect", value)]).into_response())
}

/// Client-side events to raise through `HX-Trigger`.
#[derive(Debug, Clone, Default)]
pub struct HxTrigger {
    events: serde_json::Map<String, serde_json::Value>,
}

impl HxTrigger {
    /// No events yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises an event with no data.
    #[must_use]
    pub fn event(mut self, name: &str) -> Self {
        self.events
            .insert(name.to_owned(), serde_json::Value::Bool(true));
        self
    }

    /// Raises an event with `data` as its detail. Data that can't be serialized
    /// raises the event with no data.
    #[must_use]
    pub fn event_with<T: Serialize>(mut self, name: &str, data: &T) -> Self {
        self.events.insert(
            name.to_owned(),
            serde_json::to_value(data).unwrap_or(serde_json::Value::Bool(true)),
        );
        self
    }

    /// The header value. `HeaderValue` only takes visible ASCII, so everything
    /// else is written as JSON `\u` escapes, which htmx decodes.
    fn header_value(&self) -> Option<HeaderValue> {
        if self.events.is_empty() {
            return None;
        }
        let raw = serde_json::Value::Object(self.events.clone()).to_string();
        let mut json = String::with_capacity(raw.len());
        for character in raw.chars() {
            if character.is_ascii() && !character.is_ascii_control() {
                json.push(character);
            } else {
                let mut units = [0_u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    let _ = write!(json, "\\u{unit:04x}");
                }
            }
        }
        HeaderValue::from_str(&json).ok()
    }

    /// Sets `HX-Trigger` on `response`.
    pub fn apply(&self, response: &mut Response) {
        if let Some(value) = self.header_value() {
            response.headers_mut().insert("hx-trigger", value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HxTrigger;

    #[test]
    fn no_events_sets_nothing() {
        assert!(HxTrigger::new().header_value().is_none());
    }

    #[test]
    fn non_ascii_is_escaped_as_valid_json() {
        let value = HxTrigger::new()
            .event("refresh")
            .event_with("toast", &serde_json::json!({"message": "Zażółć 🎉"}))
            .header_value()
            .unwrap();
        let text = value.to_str().unwrap();
        assert!(text.is_ascii());
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["toast"]["message"], "Zażółć 🎉");
        assert_eq!(parsed["refresh"], true);
    }
}
