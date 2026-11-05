//! Filters and functions shared by app templates. Tera 2 no longer ships
//! `urlencode`, so it lives here too.

use std::sync::Arc;

use tera::{Kwargs, State, Tera, TeraResult};
use web_mev_core::{Clock, SystemClock};

/// Settings for [`register_filters`].
#[derive(Clone)]
pub struct FilterOptions {
    /// Text after a relative age, such as `" ago"` in "5m ago".
    pub ago_suffix: String,
    /// What `ago` measures against.
    pub clock: Arc<dyn Clock>,
}

impl Default for FilterOptions {
    fn default() -> Self {
        Self {
            ago_suffix: " ago".to_owned(),
            clock: Arc::new(SystemClock),
        }
    }
}

/// Short age text: "just now", then minutes, hours, days up to two weeks, then
/// weeks, each followed by `suffix`. A future instant reads as "just now".
#[must_use]
pub fn relative_age(seconds: i64, suffix: &str) -> String {
    match seconds {
        ..60 => "just now".to_owned(),
        60..3_600 => format!("{}m{suffix}", seconds / 60),
        3_600..86_400 => format!("{}h{suffix}", seconds / 3_600),
        86_400..1_209_600 => format!("{}d{suffix}", seconds / 86_400),
        _ => format!("{}w{suffix}", seconds / 604_800),
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters, so a space
/// becomes `%20`. The output is safe in any part of a URL, including its path.
fn urlencode(value: &str, _: Kwargs, _: &State) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// A unix timestamp as `05 Nov 2026, 14:30 UTC`.
fn utc(value: i64, _: Kwargs, _: &State) -> String {
    time::OffsetDateTime::from_unix_timestamp(value)
        .ok()
        .and_then(|at| {
            at.format(time::macros::format_description!(
                "[day] [month repr:short] [year], [hour]:[minute] UTC"
            ))
            .ok()
        })
        .unwrap_or_else(|| value.to_string())
}

/// A unix timestamp as RFC 3339, for `<time datetime>`.
fn iso(value: i64, _: Kwargs, _: &State) -> String {
    time::OffsetDateTime::from_unix_timestamp(value)
        .ok()
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// Registers the `urlencode`, `utc`, `iso` and `ago` filters.
pub fn register_filters(tera: &mut Tera, options: &FilterOptions) {
    tera.register_filter("urlencode", urlencode);
    tera.register_filter("utc", utc);
    tera.register_filter("iso", iso);
    let suffix = options.ago_suffix.clone();
    let clock = options.clock.clone();
    tera.register_filter("ago", move |value: i64, _: Kwargs, _: &State| {
        relative_age(clock.unix() - value, &suffix)
    });
}

/// Registers `static_url(name="app.css")`, which returns `url(name)`. Pass the
/// app's asset URL builder.
pub fn register_static_url(tera: &mut Tera, url: impl Fn(&str) -> String + Send + Sync + 'static) {
    tera.register_function(
        "static_url",
        move |kwargs: Kwargs, _: &State| -> TeraResult<String> {
            let name: &str = kwargs.must_get("name")?;
            Ok(url(name))
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tera::{Context, Tera};
    use web_mev_core::FixedClock;

    use super::{FilterOptions, register_filters, register_static_url, relative_age};

    fn render(template: &str, value: i64, options: &FilterOptions) -> String {
        let mut tera = Tera::new();
        register_filters(&mut tera, options);
        register_static_url(&mut tera, |name| format!("/static/{name}?v=1"));
        tera.add_raw_template("t", template).unwrap();
        let mut context = Context::new();
        context.insert("value", &value);
        context.insert("text", "a b/ż&c");
        tera.render("t", &context).unwrap()
    }

    #[test]
    fn urlencode_uses_percent_twenty_and_covers_utf8() {
        assert_eq!(
            render("{{ text | urlencode }}", 0, &FilterOptions::default()),
            "a%20b%2F%C5%BC%26c"
        );
    }

    #[test]
    fn dates_format_in_utc() {
        let options = FilterOptions::default();
        assert_eq!(
            render("{{ value | utc }}", 1_793_000_000, &options),
            "26 Oct 2026, 07:33 UTC"
        );
        assert_eq!(
            render("{{ value | iso }}", 1_793_000_000, &options),
            "2026-10-26T07:33:20Z"
        );
    }

    #[test]
    fn ago_reads_the_clock_and_suffix() {
        let options = FilterOptions {
            ago_suffix: String::new(),
            clock: Arc::new(FixedClock::at(10_000)),
        };
        assert_eq!(render("{{ value | ago }}", 9_700, &options), "5m");
        let options = FilterOptions {
            clock: Arc::new(FixedClock::at(10_000)),
            ..FilterOptions::default()
        };
        assert_eq!(render("{{ value | ago }}", 9_700, &options), "5m ago");
    }

    #[test]
    fn relative_ages_read_naturally() {
        for (seconds, expected) in [
            (-5, "just now"),
            (59, "just now"),
            (60, "1m ago"),
            (3_600, "1h ago"),
            (86_400, "1d ago"),
            (1_209_600, "2w ago"),
        ] {
            assert_eq!(relative_age(seconds, " ago"), expected, "{seconds}");
        }
    }

    #[test]
    fn static_url_uses_the_apps_builder() {
        assert_eq!(
            render(
                r#"{{ static_url(name="app.css") }}"#,
                0,
                &FilterOptions::default()
            ),
            "/static/app.css?v=1"
        );
    }
}
