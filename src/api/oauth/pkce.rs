//! PKCE helpers at the OAuth boundary.
//!
//! The cryptographic primitive lives in `application::crypto` (domain-facing
//! code must not depend on the API layer); this module exposes the request-side
//! rules Mastodon enforces: only `S256` is accepted, and a challenge length
//! bound matching RFC 7636.

pub use crate::application::crypto::{pkce_s256_challenge, pkce_verify_s256};

/// The only accepted `code_challenge_method` (Mastodon configures S256 only).
pub const SUPPORTED_METHOD: &str = "S256";

/// Whether a client-supplied `code_challenge_method` is acceptable.
pub fn is_supported_method(method: &str) -> bool {
    method.eq_ignore_ascii_case(SUPPORTED_METHOD)
}

/// Basic RFC 7636 shape check for a `code_challenge`.
pub fn is_valid_challenge(challenge: &str) -> bool {
    let len = challenge.len();
    (43..=128).contains(&len)
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_s256_is_supported() {
        assert!(is_supported_method("S256"));
        assert!(is_supported_method("s256"));
        assert!(!is_supported_method("plain"));
    }

    #[test]
    fn challenge_shape_is_bounded() {
        assert!(is_valid_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(is_valid_challenge(&"a".repeat(43)));
        assert!(!is_valid_challenge("short"));
        assert!(!is_valid_challenge(&"a".repeat(129)));
    }
}
