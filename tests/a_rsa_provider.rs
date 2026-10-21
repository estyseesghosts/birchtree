//! Work package 4 (provider replacement): AWS-LC RSA behind the vendored adapter.
//!
//! Fixtures are synthetic keys generated for these tests only (`openssl genrsa`,
//! PKCS#8 / SubjectPublicKeyInfo as stored and published by birchtree); the
//! signature below was produced by OpenSSL, an independent implementation.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use activitypub_federation::http_signatures::{Keypair, generate_actor_keypair};
use activitypub_federation::rsa_provider::{SigningKey, verify_legacy_rsa_sha256};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as Base64;

use birchtree::activitypub::key_generation;
use birchtree::application::federation_ports::FederationStore;
use birchtree::domain::Id;

mod common;

const STORED_PRIVATE_KEY: &str = concat!(
    "-----BEGIN PRIVATE KEY-----
",
    "MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCXMjrc1BE67KkW
",
    "T0cd58b+MvqrzaI9DmIBdOoVVQAvm28ap13+FBDrwH8ihQX37cPLtCx13nohQvbV
",
    "bBcCroD/2qdn24A7HaOSCYbeWWaYrBxaVVt6WpA7SuLXW4ON2x5CGqlg7jE8wzWI
",
    "+NIcqruwSEUzvfUNSpXK7KqCtJHpZ35DWgPXSLG2erNK1RT1aR/CUG9gcCAz/pWD
",
    "1KNErUk9wdVtFJaXxKqVllIRXzIMcFy6MbN0U81thGMfP9RWU6wSNkVBqR+09eNv
",
    "CO9ccbf6KikBCo4RM7sixGLP+bz64UM1pqugQ3mAp8euk6/YvOd+F4owVACoUWMz
",
    "DH/hq+J3AgMBAAECggEAI0Hc1RMEw5NdaKzq/g/G6Q8VQkvs+cTpWMKdNYVBSli6
",
    "I2KE+hNO0NLuMqwHdMW2o0FsHr5XB3uYmLMgeIkd3svrvr3CXMawFoVvNFlvocl8
",
    "wRnnf5QvzwvK8wJNnp5Ko4qZFdfllnBFAyBWysWLh/OKY6L3ynYqkQv4RQt7fQw/
",
    "7wacJip6eENaKjpN2AszZLVmPhWoiRdCzskPJsY7EdTT4tUz1fBW/1HV7mvRCidv
",
    "hOCYQz+YSbEO0/izFn/SmUbhQr/rxPmEGEdczXBn7Ie2JWUTVSB+g+NPdlChewdc
",
    "h3D48cDhYQYzyC9J45dgco7hWNefJblIDdiMeJ39iQKBgQDJUkF3t2Njbl/sUFPq
",
    "O6dpf8YPIi4o7zm//uhCyVouBhGXcSIYakRI8Bol1dqxiWGhMQVQDqqTR5dHM/Mi
",
    "U9rdjzFG+P8gbTok+pDAu+ycawX9EJ+FAQJ30CiWxIio8Wwezch+a23J8rKYtine
",
    "Ebrw91IhNOsxEjgSY9Pe91iDzwKBgQDAQs60iD6/GMs6IhrZdbRJdEDOOS8I+G/o
",
    "QTExGpJeqkbSwdPYAOxz6TI2S6/LzPbVh0dSY4+n5amxjpb18vqCbR4guNxGip/c
",
    "RoexP2sP1ZpXBXqgf7us900Agd78eUY/DUK8YiQni3xPZw3DFq4jsFEkRfpUH9Ho
",
    "uUzxRJtY2QKBgHaBtwvzpcu4AzsmJd9x8iBGGJ577lQmTgZY1PvLbww8gkp1TC63
",
    "Dy8oNxwigLOVPSUXqBryJzK/J6vARtTkK6fUTJuMoxHBekKeRGtPs/53eXT4Pg6b
",
    "A4rnu5zUX+WjB5Qyal0Qe/YuqTb/tDGGmlaSH4RwlCm0odEhdmx/iVk9AoGAQte3
",
    "R9rKfePf3CE5Vntup88MLOaCrMq7FXN858aGNvVaVDHhNgr0KkRcGbZssx1af9hd
",
    "eo50xnm5aqrZXz8Py0Kmo9fBdaECfTHkQUJ8NXAdDb4w6RYfqL39/nwoi3yaaJdy
",
    "qU4xpbIAERcSGM6M0omdXFAb/R1m/FNYJdWlcNkCgYAaouXT4+eP9MhjQ9+HxhgV
",
    "PTx5F3LqI26VR4z88kdr3rn/fT9lEcLibS5fRkhdDK1ainfQaun7gl3NB4aLnunG
",
    "u7G48r/MljvI1N28hXSSqIF+pYjjvaA9xJ6/FqsTxNINn3tS/nt7NDkvNIiXwFt9
",
    "ivuh4EQk7NL3ukAxNIxD6w==
",
    "-----END PRIVATE KEY-----
",
);
const PUBLISHED_PUBLIC_KEY: &str = concat!(
    "-----BEGIN PUBLIC KEY-----
",
    "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAlzI63NQROuypFk9HHefG
",
    "/jL6q82iPQ5iAXTqFVUAL5tvGqdd/hQQ68B/IoUF9+3Dy7Qsdd56IUL21WwXAq6A
",
    "/9qnZ9uAOx2jkgmG3llmmKwcWlVbelqQO0ri11uDjdseQhqpYO4xPMM1iPjSHKq7
",
    "sEhFM731DUqVyuyqgrSR6Wd+Q1oD10ixtnqzStUU9WkfwlBvYHAgM/6Vg9SjRK1J
",
    "PcHVbRSWl8SqlZZSEV8yDHBcujGzdFPNbYRjHz/UVlOsEjZFQakftPXjbwjvXHG3
",
    "+iopAQqOETO7IsRiz/m8+uFDNaaroEN5gKfHrpOv2LznfheKMFQAqFFjMwx/4avi
",
    "dwIDAQAB
",
    "-----END PUBLIC KEY-----
",
);
const EC_PUBLIC_KEY: &str = concat!(
    "-----BEGIN PUBLIC KEY-----
",
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEk06P+xArNY6J2joIvb6Xjvtu1tDf
",
    "PrE0PdbBvOeIxTc/yPmXRBusMGY9CJx3hVH/Kcf6TcTlQhB4uZeAmodisw==
",
    "-----END PUBLIC KEY-----
",
);

/// `openssl dgst -sha256 -sign` over `SIGNED_MESSAGE` with the stored key.
const OPENSSL_SIGNATURE: &str = "JoRa7oqlUv8EnUB3B5/L6bSkLyAeRFkFByqTQVitNi3ImDRbXv52J/d+sYKlBxrM0RcnvU8XscJckLc+p+eDM0M7h4mcNgY3na0jRRtS0M8XyK1JFLQAgGIvwn7aeSnUIErv5R/xDR9ER+SLwGoEK3+fLuvHp1XgMCKmQT3jYdPDxGSOo/c1ocd2W/jkbEdtG8HJEQNA7np6IhBtYa1jEMLHIrI8wpqRb1HwAOseANgWuTSRbOSokO5b8ELy+vyy0D8qECM/bhcLZae8tHnObzG0VhWsL3wsaR1kaJqgh1SS5dblJOy8gTmsXq9NHe0oU670MKui1/tDj84xuHTwrA==";
const SIGNED_MESSAGE: &[u8] = b"legacy signing string";

#[test]
fn stored_pkcs8_key_loads_and_matches_the_published_public_key() {
    let key = SigningKey::from_pkcs8_pem(STORED_PRIVATE_KEY).expect("stored key imports");
    assert_eq!(
        key.public_key_pem().expect("public key"),
        PUBLISHED_PUBLIC_KEY,
        "the published SPKI PEM is reproduced exactly, so keys never rotate"
    );
}

#[test]
fn signature_is_byte_identical_to_an_independent_pkcs1_v15_sha256_signature() {
    let key = SigningKey::from_pkcs8_pem(STORED_PRIVATE_KEY).expect("stored key imports");
    let signature = key.sign_legacy_rsa_sha256(SIGNED_MESSAGE).expect("sign");
    assert_eq!(
        Base64.encode(&signature),
        OPENSSL_SIGNATURE,
        "the signing string is hashed once, inside the operation"
    );
}

#[test]
fn openssl_signature_verifies_with_the_published_key() {
    let signature = Base64.decode(OPENSSL_SIGNATURE).expect("base64");
    assert!(
        verify_legacy_rsa_sha256(PUBLISHED_PUBLIC_KEY, SIGNED_MESSAGE, &signature).expect("verify")
    );
    assert!(
        !verify_legacy_rsa_sha256(PUBLISHED_PUBLIC_KEY, b"tampered", &signature).expect("verify")
    );
    let mut flipped = signature.clone();
    flipped[0] ^= 1;
    assert!(
        !verify_legacy_rsa_sha256(PUBLISHED_PUBLIC_KEY, SIGNED_MESSAGE, &flipped).expect("verify")
    );
}

#[test]
fn public_key_with_a_trailing_blank_line_still_verifies() {
    let signature = Base64.decode(OPENSSL_SIGNATURE).expect("base64");
    let akkoma_style = format!(
        "{PUBLISHED_PUBLIC_KEY}
"
    );
    assert!(verify_legacy_rsa_sha256(&akkoma_style, SIGNED_MESSAGE, &signature).expect("verify"));
}

#[test]
fn unusable_public_keys_are_errors_not_successes() {
    let signature = Base64.decode(OPENSSL_SIGNATURE).expect("base64");
    assert!(verify_legacy_rsa_sha256("", SIGNED_MESSAGE, &signature).is_err());
    assert!(verify_legacy_rsa_sha256("not a key", SIGNED_MESSAGE, &signature).is_err());
    assert!(
        verify_legacy_rsa_sha256(EC_PUBLIC_KEY, SIGNED_MESSAGE, &signature).is_err(),
        "an EC key is not accepted for rsa-sha256"
    );
}

#[test]
fn malformed_private_keys_fail_import_instead_of_being_replaced() {
    assert!(SigningKey::from_pkcs8_pem("").is_err());
    assert!(
        SigningKey::from_pkcs8_pem(
            "-----BEGIN PRIVATE KEY-----
AAAA
-----END PRIVATE KEY-----
"
        )
        .is_err()
    );
    assert!(
        SigningKey::from_pkcs8_pem(PUBLISHED_PUBLIC_KEY).is_err(),
        "a public key is not a private key"
    );
}

#[test]
fn generated_actor_keys_use_the_storage_and_publication_formats() {
    let pair = generate_actor_keypair().expect("generate");
    assert!(pair.private_key.starts_with(
        "-----BEGIN PRIVATE KEY-----
"
    ));
    assert!(pair.public_key.starts_with(
        "-----BEGIN PUBLIC KEY-----
"
    ));
    let key = SigningKey::from_pkcs8_pem(&pair.private_key).expect("import");
    assert_eq!(key.public_key_pem().expect("public key"), pair.public_key);
    let signature = key.sign_legacy_rsa_sha256(b"hello").expect("sign");
    assert_eq!(signature.len(), 256, "2048-bit modulus");
    assert!(verify_legacy_rsa_sha256(&pair.public_key, b"hello", &signature).expect("verify"));
}

/// A counting stand-in for key generation, so the test measures how often it runs.
fn counting_generator(
    counter: Arc<AtomicUsize>,
) -> impl FnOnce() -> Result<Keypair, activitypub_federation::error::Error> + Send + 'static {
    move || {
        counter.fetch_add(1, Ordering::SeqCst);
        generate_actor_keypair()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_requests_generate_one_key_and_all_get_the_winner() {
    let (_dir, store) = common::temp_store();
    let account_id = Id::new(4242);
    let generations = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        let generations = generations.clone();
        tasks.push(tokio::spawn(async move {
            key_generation::ensure_keys(
                &*store,
                account_id,
                "https://example.test/users/a#main-key",
                counting_generator(generations),
            )
            .await
            .expect("keys")
        }));
    }
    let mut public_keys = Vec::new();
    for task in tasks {
        public_keys.push(task.await.expect("join").public_key_pem);
    }
    assert_eq!(
        generations.load(Ordering::SeqCst),
        1,
        "no key-generation storm"
    );
    assert!(
        public_keys.windows(2).all(|pair| pair[0] == pair[1]),
        "every caller sees the winner"
    );
    let stored = store.actor_keys(account_id).expect("read").expect("stored");
    assert_eq!(stored.public_key_pem, public_keys[0]);
}

#[test]
fn store_local_keys_returns_the_winner_and_discards_the_loser() {
    let (_dir, store) = common::temp_store();
    let account_id = Id::new(777);
    let first = generate_actor_keypair().expect("first");
    let second = generate_actor_keypair().expect("second");
    let won = store
        .store_local_keys(
            account_id,
            "k#main-key",
            &first.public_key,
            &first.private_key,
        )
        .expect("store");
    let lost = store
        .store_local_keys(
            account_id,
            "k#main-key",
            &second.public_key,
            &second.private_key,
        )
        .expect("store");
    assert_eq!(won.public_key_pem, first.public_key);
    assert_eq!(
        lost.public_key_pem, first.public_key,
        "the loser receives the stored pair"
    );
    assert_eq!(
        lost.private_key_pem.as_deref(),
        Some(first.private_key.as_str())
    );
}
