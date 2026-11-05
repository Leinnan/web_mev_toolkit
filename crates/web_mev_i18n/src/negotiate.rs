//! Choosing a request's language, as plain functions of the raw header values.

use crate::LocaleCode;

/// The language of a request: the cookie if it names a supported locale, then the
/// best supported `Accept-Language` entry, then `default`. A regional request such
/// as `pl-PL` matches a plain `pl`.
#[must_use]
pub fn negotiate(
    cookie: Option<&str>,
    accept_language: Option<&str>,
    supported: &[LocaleCode],
    default: &LocaleCode,
) -> LocaleCode {
    cookie
        .and_then(|value| match_supported(value, supported))
        .or_else(|| accept_language.and_then(|value| best_accepted(value, supported)))
        .unwrap_or_else(|| default.clone())
}

fn match_supported(code: &str, supported: &[LocaleCode]) -> Option<LocaleCode> {
    let wanted = LocaleCode::try_from(code).ok()?;
    let plain = wanted.without_region();
    supported
        .iter()
        .find(|locale| **locale == wanted)
        .or_else(|| supported.iter().find(|locale| **locale == plain))
        .cloned()
}

/// The supported entry with the highest weight. Earlier entries win ties, and a
/// weight of zero means "not this one".
fn best_accepted(header: &str, supported: &[LocaleCode]) -> Option<LocaleCode> {
    let mut best: Option<(f32, LocaleCode)> = None;
    for entry in header.split(',') {
        let mut parts = entry.split(';').map(str::trim);
        let code = parts.next().unwrap_or_default();
        let weight = parts
            .find_map(|parameter| parameter.strip_prefix("q="))
            .map_or(Some(1.0), |raw| raw.parse::<f32>().ok());
        let Some(weight) = weight.filter(|weight| *weight > 0.0) else {
            continue;
        };
        if best.as_ref().is_some_and(|(current, _)| *current >= weight) {
            continue;
        }
        if let Some(locale) = match_supported(code, supported) {
            best = Some((weight, locale));
        }
    }
    best.map(|(_, locale)| locale)
}

/// `candidate` if it is safe to redirect to after switching language, and `/`
/// otherwise. Only same-site absolute paths pass: browsers read both `//host` and
/// `/\host` as another site.
#[must_use]
pub fn safe_return_path(candidate: Option<&str>) -> String {
    match candidate {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.starts_with("/\\")
                && !path.chars().any(char::is_control) =>
        {
            path.to_owned()
        }
        _ => "/".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{negotiate, safe_return_path};
    use crate::LocaleCode;

    fn locale(code: &str) -> LocaleCode {
        LocaleCode::try_from(code).unwrap()
    }

    fn pick(cookie: Option<&str>, header: Option<&str>) -> String {
        negotiate(cookie, header, &[locale("en"), locale("pl")], &locale("en")).code()
    }

    #[test]
    fn cookie_beats_the_header() {
        assert_eq!(pick(Some("pl"), Some("en")), "pl");
    }

    #[test]
    fn unusable_cookie_falls_through() {
        assert_eq!(pick(Some("de"), Some("pl")), "pl");
        assert_eq!(pick(Some("<script>"), Some("pl")), "pl");
        assert_eq!(pick(None, None), "en");
        assert_eq!(pick(Some("de"), None), "en");
    }

    #[test]
    fn header_is_read_by_weight() {
        assert_eq!(pick(None, Some("de, pl;q=0.8, en;q=0.5")), "pl");
        assert_eq!(pick(None, Some("en;q=0.2, pl;q=0.9")), "pl");
        assert_eq!(pick(None, Some("pl, en")), "pl", "earlier wins a tie");
    }

    #[test]
    fn zero_or_broken_weights_are_skipped() {
        assert_eq!(pick(None, Some("pl;q=0")), "en");
        assert_eq!(pick(None, Some("pl;q=high")), "en");
    }

    #[test]
    fn regions_match_their_language() {
        assert_eq!(pick(None, Some("pl-PL,pl;q=0.9")), "pl");
        assert_eq!(pick(Some("pl_PL"), None), "pl");
        assert_eq!(pick(None, Some("*")), "en");
    }

    #[test]
    fn an_exact_region_is_preferred() {
        let supported = [locale("pt"), locale("pt-br")];
        assert_eq!(
            negotiate(None, Some("pt-BR"), &supported, &locale("pt")).code(),
            "pt-br"
        );
    }

    #[test]
    fn return_paths_stay_on_this_site() {
        assert_eq!(safe_return_path(Some("/photos?page=2")), "/photos?page=2");
        for unsafe_path in [
            "//evil.example",
            "/\\evil.example",
            "https://evil.example",
            "evil",
            "",
            "/a\r\nSet-Cookie: x=1",
        ] {
            assert_eq!(safe_return_path(Some(unsafe_path)), "/", "{unsafe_path:?}");
        }
        assert_eq!(safe_return_path(None), "/");
    }
}
