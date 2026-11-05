//! Random tokens, their stored form, and constant-time comparison.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// URL-safe Base64 of `bytes` random bytes.
#[must_use]
pub fn random_token(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(2 * digest.len());
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The form of a token stored in the database. Raw tokens are never persisted.
#[must_use]
pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Compares two strings without leaking where they differ. Their lengths can
/// still leak, so compare values of a fixed length, such as hashes or tokens.
#[must_use]
pub fn secure_eq(left: &str, right: &str) -> bool {
    left.len() == right.len() && left.as_bytes().ct_eq(right.as_bytes()).into()
}

/// The credential of a raw `Authorization: Bearer ...` value. The scheme is
/// case-insensitive, and an empty credential counts as none.
#[must_use]
pub fn bearer(authorization: Option<&str>) -> Option<&str> {
    let (scheme, credential) = authorization?.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    Some(credential.trim()).filter(|credential| !credential.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{bearer, random_token, secure_eq, sha256_hex, token_hash};

    #[test]
    fn tokens_are_url_safe_and_differ() {
        let a = random_token(32);
        let b = random_token(32);
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn hash_is_lowercase_sha256_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(token_hash("abc"), sha256_hex(b"abc"));
    }

    #[test]
    fn secure_eq_compares_content_and_length() {
        assert!(secure_eq("abc", "abc"));
        assert!(!secure_eq("abc", "abd"));
        assert!(!secure_eq("abc", "abcd"));
        assert!(secure_eq("", ""));
    }

    #[test]
    fn bearer_parses_the_header() {
        assert_eq!(bearer(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer(Some("bEaReR  abc ")), Some("abc"));
        assert_eq!(bearer(Some("Basic abc")), None);
        assert_eq!(bearer(Some("Bearer ")), None);
        assert_eq!(bearer(Some("Bearer")), None);
        assert_eq!(bearer(None), None);
    }
}
