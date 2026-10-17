//! Persistence conformance suite (docs/persistence-conformance.md).
//!
//! Runs against the real `redb` adapter (no mocks). Covers unique indexes,
//! counter/index consistency, transaction atomicity, migrations, dedup and job
//! lease/retry/quarantine, and read-after-commit.

mod common;

use std::sync::Arc;

use birchtree::application::auth;
use birchtree::application::ports::{
    AccountStore, JobQueue, OAuthStore, SessionStore,
};
use birchtree::application::crypto;
use birchtree::domain::{JobKind, JobState, LocalUser};
use birchtree::domain::job::LEASE_MS;

use common::{mk_account, open_store, temp_store, T0};

#[test]
fn create_and_read_back_account_and_credential() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "Alice");

    let by_id = store.account_by_id(account.id).unwrap().unwrap();
    assert_eq!(by_id.username, "Alice");
    assert_eq!(by_id.normalized_username, "alice");
    assert!(by_id.local);

    let by_handle = store
        .account_by_handle("alice", "example.social")
        .unwrap()
        .unwrap();
    assert_eq!(by_handle.id, account.id);

    let user: LocalUser = store.user_by_account_id(account.id).unwrap().unwrap();
    assert!(user.password_hash.starts_with("$argon2"));
    assert_eq!(store.account_count().unwrap(), 1);
}

#[test]
fn duplicate_handle_is_rejected_case_insensitively() {
    let (_dir, store) = temp_store();
    mk_account(&store, "Alice");

    // Same normalization ("alice") but different case must collide.
    let second = create_named(&store, "alice");
    assert!(second.is_err(), "expected duplicate handle rejection");
    assert_eq!(store.account_count().unwrap(), 1);
}

fn create_named(store: &birchtree::persistence::redb::RedbStore, name: &str) -> birchtree::domain::Result<()> {
    birchtree::application::accounts::create_account(
        store,
        "https://example.social",
        T0,
        birchtree::application::accounts::CreateAccountCommand {
            username: name.to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .map(|_| ())
}

#[test]
fn unique_handle_race_has_exactly_one_winner() {
    let (_dir, store) = temp_store();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        handles.push(std::thread::spawn(move || create_named(&store, "racer").is_ok()));
    }
    let winners = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "exactly one concurrent create must win");
    assert_eq!(store.account_count().unwrap(), 1);
}

#[test]
fn password_authentication() {
    let (_dir, store) = temp_store();
    mk_account(&store, "alice");

    let ok = auth::authenticate_password(&*store, "alice", "example.social", "password123").unwrap();
    assert!(ok.is_some());

    let wrong = auth::authenticate_password(&*store, "alice", "example.social", "nope").unwrap();
    assert!(wrong.is_none());

    let missing =
        auth::authenticate_password(&*store, "ghost", "example.social", "password123").unwrap();
    assert!(missing.is_none());
}

#[test]
fn oauth_app_and_token_lifecycle() {
    let (_dir, store) = temp_store();
    let registered = auth::register_app(
        &*store,
        "Test Client".into(),
        vec!["https://client.example/cb".into()],
        vec!["read".into(), "write".into()],
        None,
        true,
        T0,
    )
    .unwrap();

    let app = store
        .app_by_client_id(&registered.app.client_id)
        .unwrap()
        .unwrap();
    assert_eq!(app.name, "Test Client");
    assert!(app.client_secret_hash.is_some());

    let user = mk_account(&store, "bob");
    let (plaintext, record) =
        auth::issue_user_token(&*store, app.id, user.id, vec!["read".into()], T0).unwrap();

    // Lookup by plaintext resolves to the record; stored hash is not plaintext.
    let found = store.token_by_plaintext(&plaintext).unwrap().unwrap();
    assert_eq!(found.id, record.id);
    assert_ne!(record.token_hash, plaintext);

    // Revocation makes the token invalid.
    assert!(store.revoke_token(&plaintext).unwrap());
    let revoked = store.token_by_plaintext(&plaintext).unwrap().unwrap();
    assert!(revoked.revoked);
    assert!(!revoked.is_valid_at(T0));
    // Revoking an unknown token returns false, not an error.
    assert!(!store.revoke_token("unknown").unwrap());
}

#[test]
fn login_session_lifecycle_and_expiry() {
    let (_dir, store) = temp_store();
    let user = mk_account(&store, "carol");
    let (plaintext, session) = auth::create_login_session(&*store, user.id, T0).unwrap();
    assert!(session.is_valid_at(T0));

    let found = store.session_by_plaintext(&plaintext).unwrap().unwrap();
    assert_eq!(found.user_id, user.id);

    // Expiry boundary is exclusive.
    assert!(!session.is_valid_at(session.expires_at_ms));

    assert!(store.revoke_session(&plaintext).unwrap());
    assert!(store.session_by_plaintext(&plaintext).unwrap().is_none());
}

#[test]
fn account_creation_commits_one_follow_up_job_atomically() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "dave");

    let jobs = store.all_jobs().unwrap();
    assert_eq!(jobs.len(), 1, "exactly one follow-up job");
    assert_eq!(jobs[0].kind, JobKind::UserCreated);
    assert_eq!(jobs[0].state, JobState::Pending);
    assert_eq!(
        jobs[0].payload.get("account_id").unwrap().as_u64().unwrap(),
        account.id.value()
    );
    assert_eq!(store.schedule_index_len().unwrap(), 1);
}

#[test]
fn job_claim_complete_retry_and_quarantine() {
    let (_dir, store) = temp_store();

    // Claim and complete.
    let job = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .unwrap();
    let claimed = store.claim_due(T0, 10).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].state, JobState::Leased);
    assert_eq!(claimed[0].attempts, 1);
    assert_eq!(claimed[0].lease_until_ms, Some(T0 + LEASE_MS));
    store.complete(job.id).unwrap();
    assert_eq!(store.get(job.id).unwrap().unwrap().state, JobState::Succeeded);
    assert_eq!(store.schedule_index_len().unwrap(), 0);

    // Retry respects the backoff window.
    let job2 = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .unwrap();
    let _ = store.claim_due(T0, 10).unwrap();
    let retried = store.retry(job2.id, "boom", T0).unwrap();
    assert_eq!(retried.state, JobState::Pending);
    assert_eq!(retried.attempts, 1);
    assert!(retried.due_at_ms > T0);
    assert!(store.claim_due(T0, 10).unwrap().is_empty(), "not due yet");
    assert_eq!(store.claim_due(retried.due_at_ms, 10).unwrap().len(), 1);
    store.complete(job2.id).unwrap();

    // Exhaust the retry budget -> quarantined.
    let job3 = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .unwrap();
    let mut now = T0;
    loop {
        let claimed = store.claim_due(now, 10).unwrap();
        if claimed.is_empty() {
            break;
        }
        let updated = store.retry(job3.id, "always fails", now).unwrap();
        if updated.state == JobState::Quarantined {
            break;
        }
        now = updated.due_at_ms;
    }
    let final_job = store.get(job3.id).unwrap().unwrap();
    assert_eq!(final_job.state, JobState::Quarantined);
    assert_eq!(final_job.last_error.as_deref(), Some("always fails"));
    assert_eq!(store.schedule_index_len().unwrap(), 0);
}

#[test]
fn migrations_and_data_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let account = {
        let store = open_store(dir.path());
        mk_account(&store, "erin")
    };

    // Reopen the same file: migration is a no-op and data persists.
    let store = open_store(dir.path());
    assert_eq!(store.account_count().unwrap(), 1);
    let read = store.account_by_id(account.id).unwrap().unwrap();
    assert_eq!(read.username, "erin");
    assert_eq!(store.all_jobs().unwrap().len(), 1);
}

#[test]
fn authorization_code_redeem_is_single_use() {
    let (_dir, store) = temp_store();
    let registered = auth::register_app(
        &*store,
        "Client".into(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .unwrap();
    let user = mk_account(&store, "frank");

    let (code, record) = auth::create_authorization_code(
        &*store,
        registered.app.id,
        user.id,
        "https://client.example/cb".into(),
        vec!["read".into()],
        Some(crypto::pkce_s256_challenge("verifier-value")),
        Some("S256".to_string()),
        T0,
    )
    .unwrap();
    assert_eq!(record.expires_at_ms, T0 + auth::AUTH_CODE_TTL_MS);

    let exchange = || {
        auth::exchange_authorization_code(
            &*store,
            &registered.app,
            auth::CodeExchange {
                code: &code,
                redirect_uri: Some("https://client.example/cb"),
                code_verifier: Some("verifier-value"),
            },
            T0,
        )
    };

    assert!(exchange().is_ok());
    // A replay can never mint a second token.
    assert!(exchange().is_err());
}

#[test]
fn authorization_code_redeem_is_atomic_under_contention() {
    let (_dir, store) = temp_store();
    let registered = auth::register_app(
        &*store,
        "Client".into(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .unwrap();
    let user = mk_account(&store, "grace");
    let (code, _record) = auth::create_authorization_code(
        &*store,
        registered.app.id,
        user.id,
        "https://client.example/cb".into(),
        vec!["read".into()],
        None,
        None,
        T0,
    )
    .unwrap();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        let app = registered.app.clone();
        let code = code.clone();
        handles.push(std::thread::spawn(move || {
            auth::exchange_authorization_code(
                &*store,
                &app,
                auth::CodeExchange {
                    code: &code,
                    redirect_uri: Some("https://client.example/cb"),
                    code_verifier: None,
                },
                T0,
            )
            .is_ok()
        }));
    }
    let winners = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "exactly one concurrent exchange must win");
}

#[test]
fn registration_commits_confirmation_mail_job_atomically() {
    let (_dir, store) = temp_store();
    let registered = auth::register_app(
        &*store,
        "Client".into(),
        vec!["https://client.example/cb".into()],
        vec!["read".into(), "write".into()],
        None,
        true,
        T0,
    )
    .unwrap();

    let result = birchtree::application::accounts::register_account(
        &*store,
        &*store,
        "https://example.social",
        T0,
        true,
        &registered.app,
        birchtree::application::accounts::RegisterAccountCommand {
            username: "heidi".into(),
            email: "heidi@example.com".into(),
            password: "password123".into(),
            agreement: true,
            locale: None,
            reason: None,
            invite_code: None,
            display_name: None,
        },
    )
    .unwrap();

    let jobs = store.all_jobs().unwrap();
    let kinds: Vec<JobKind> = jobs.iter().map(|j| j.kind).collect();
    assert!(kinds.contains(&JobKind::UserCreated));
    assert!(kinds.contains(&JobKind::SendConfirmationEmail));

    let mail_job = jobs
        .iter()
        .find(|j| j.kind == JobKind::SendConfirmationEmail)
        .unwrap();
    assert_eq!(
        mail_job.payload.get("email").unwrap().as_str().unwrap(),
        "heidi@example.com"
    );
    assert_eq!(result.token.scopes, vec!["read".to_string(), "write".to_string()]);
}

#[test]
fn crypto_helpers_reject_tampered_hash() {
    let hash = crypto::hash_password("hunter2hunter2").unwrap();
    assert!(crypto::verify_password(&hash, "hunter2hunter2"));
    assert!(!crypto::verify_password("not-a-phc-string", "hunter2hunter2"));
}
