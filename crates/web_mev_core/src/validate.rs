//! Validators for form input. Error messages are safe to show to the user.

/// Input that failed validation. The message is safe to display.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub String);

/// `value` trimmed, which has to hold 1 to `maximum` characters.
///
/// # Errors
///
/// Returns [`ValidationError`], naming `field`, when it doesn't.
pub fn required_text<'a>(
    field: &str,
    value: &'a str,
    maximum: usize,
) -> Result<&'a str, ValidationError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum {
        return Err(ValidationError(format!(
            "{field} must contain 1 to {maximum} characters"
        )));
    }
    Ok(value)
}

/// Longest slug accepted by [`validate_slug`] and produced by [`slugify`].
pub const SLUG_MAX_LEN: usize = 50;

/// Checks a slug: 2 to 50 lowercase ASCII letters, digits or single dashes,
/// with no dash at either end.
///
/// # Errors
///
/// Returns [`ValidationError`] when `slug` breaks any of these.
pub fn validate_slug(slug: &str) -> Result<(), ValidationError> {
    if slug.len() < 2
        || slug.len() > SLUG_MAX_LEN
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ValidationError(format!(
            "slug must contain 2 to {SLUG_MAX_LEN} lowercase letters, digits, or dashes"
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return Err(ValidationError(
            "slug must not start or end with a dash or contain consecutive dashes".into(),
        ));
    }
    Ok(())
}

/// Builds a slug from free text. Anything that isn't ASCII alphanumeric becomes
/// a separator. The result can be empty or a single character, so check it with
/// [`validate_slug`].
#[must_use]
pub fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(SLUG_MAX_LEN);
    let mut pending_separator = false;
    for character in value.chars() {
        if !character.is_ascii_alphanumeric() {
            pending_separator = true;
            continue;
        }
        if slug.len() + 1 > SLUG_MAX_LEN {
            break;
        }
        if pending_separator && !slug.is_empty() && slug.len() + 2 <= SLUG_MAX_LEN {
            slug.push('-');
        }
        pending_separator = false;
        slug.push(character.to_ascii_lowercase());
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::{required_text, slugify, validate_slug};

    #[test]
    fn required_text_trims_and_counts_characters() {
        assert_eq!(required_text("name", "  ok ", 5).unwrap(), "ok");
        assert!(required_text("name", "   ", 5).is_err());
        assert_eq!(required_text("name", "żółty", 5).unwrap(), "żółty");
        assert!(
            required_text("name", "żółtyy", 5)
                .unwrap_err()
                .0
                .contains("name")
        );
    }

    #[test]
    fn slugs_are_strict() {
        assert!(validate_slug("my-app-2").is_ok());
        for bad in [
            "a",
            "-ab",
            "ab-",
            "a--b",
            "Ab",
            "a b",
            "ąb",
            &"a".repeat(51),
        ] {
            assert!(validate_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn slugify_collapses_separators_and_respects_bounds() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("--Hello--World--"), "hello-world");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("Ünïcödé"), "n-c-d");
        assert_eq!(slugify(&"a".repeat(60)), "a".repeat(50));
        let forty_nine = "a".repeat(49);
        assert_eq!(
            slugify(&format!("{forty_nine}-b")),
            format!("{forty_nine}b")
        );
    }
}
