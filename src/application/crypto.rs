//! Password and token cryptography.
//!
//! Passwords are hashed with Argon2 (PHC string). Opaque tokens (client
//! secrets, access tokens, login sessions) are stored only as a SHA-256 hex
//! digest; plaintext is generated here and handed to the caller once.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Hash a password into an Argon2 PHC string.
pub fn hash_password(plaintext: &str) -> Result<String, String> {
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

/// Verify a password against an Argon2 PHC string. Constant-time within Argon2.
pub fn verify_password(hash: &str, plaintext: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(plaintext.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Hex-encoded SHA-256 digest, used as the storage key for opaque tokens.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Generate a random hex token of `n` bytes (2n hex characters).
pub fn random_token_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// A fixed valid Argon2 hash used to equalize timing for unknown accounts.
///
/// Verifying against this value costs the same as a real check, so a login
/// attempt for a nonexistent user does not return measurably faster.
pub fn dummy_verify(plaintext: &str) {
    // "password" hashed with Argon2id, generated once for timing equalization.
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHR2YWx1ZTEy$2n0nBwP2hkWyBd9p7mBqXPnKzKqM1WPToOAEe0n8Y3E";
    let _ = verify_password(DUMMY, plaintext);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_round_trips() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password(&hash, "correct horse battery staple"));
        assert!(!verify_password(&hash, "wrong password"));
    }

    #[test]
    fn hashes_are_salted() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password(&a, "same"));
        assert!(verify_password(&b, "same"));
    }

    #[test]
    fn token_hash_is_deterministic_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn random_tokens_are_unique_and_hex() {
        let a = random_token_hex(32);
        let b = random_token_hex(32);
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}
