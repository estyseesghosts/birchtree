//! Secret-handling tests.
//!
//! Plaintext passwords, tokens, and session secrets must never be persisted or
//! serialized outward. The database stores only Argon2 hashes and SHA-256
//! token digests.

mod common;

use birchtree::application::auth;
use birchtree::domain::Account;
use birchtree::observability;

use common::{mk_account, temp_store, T0};

fn db_bytes(dir: &std::path::Path) -> Vec<u8> {
    std::fs::read(dir.join("birchtree.redb")).expect("read db file")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn database_never_contains_plaintext_secrets() {
    let (dir, store) = temp_store();

    let account: Account = mk_account(&store, "secadmin");
    let _ = account;

    // Issue a token and a session; keep the plaintexts in memory only.
    let registered = auth::register_app(
        &*store,
        "client".into(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        false,
        T0,
    )
    .unwrap();
    let (token_plaintext, token_record) = auth::issue_user_token(
        &*store,
        registered.app.id,
        account.id,
        vec!["read".into()],
        T0,
    )
    .unwrap();
    let (session_plaintext, _session) = auth::create_login_session(&*store, account.id, T0).unwrap();

    // Release the store before reading the database file back: redb holds a
    // file lock while open, which Windows (unlike Linux) refuses to read
    // through. All writes above are committed, so the bytes are stable.
    drop(store);

    let bytes = db_bytes(dir.path());

    // The password used to create the account is "password123" (see common);
    // its plaintext must never reach the database.
    assert!(!contains(&bytes, "password123"), "account password leaked");
    assert!(
        !contains(&bytes, &token_plaintext),
        "access token plaintext leaked"
    );
    assert!(
        !contains(&bytes, &session_plaintext),
        "session token plaintext leaked"
    );

    // The token digest is what is stored, and it differs from the plaintext.
    assert_ne!(token_record.token_hash, token_plaintext);
    assert!(
        contains(&bytes, &token_record.token_hash),
        "token hash should be persisted"
    );
}

#[test]
fn serialized_token_record_omits_plaintext() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "serial");
    let registered = auth::register_app(
        &*store,
        "client".into(),
        vec![],
        vec![],
        None,
        false,
        T0,
    )
    .unwrap();
    let (plaintext, record) =
        auth::issue_user_token(&*store, registered.app.id, account.id, vec![], T0).unwrap();

    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains(&plaintext));
    assert!(json.contains(&record.token_hash));
}

#[test]
fn authorization_headers_are_redacted() {
    assert_eq!(
        observability::redact_authorization("Bearer abcdef123456"),
        "Bearer [redacted]"
    );
    assert_eq!(observability::redact("secret"), "[redacted]");
}
