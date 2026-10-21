//! Work packages 1 and 11 (agent B): email-token secrecy, atomic redemption,
//! sealed TOTP seeds, the mail-outbox lifecycle and the WebAuthn account
//! index. Everything runs against the real `redb` store.

mod common;

use std::sync::{Arc, Mutex};

use birchtree::application::accounts::{confirm_email, reset_password, SecondFactorStore};
use birchtree::application::crypto;
use birchtree::application::ports::{AccountStore, JobQueue, MailSender, NewLocalAccount, NoStreaming};
use birchtree::domain::user::{
    EmailToken, EmailTokenKind, TotpRecord, WebauthnCredential,
    MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT,
};
use birchtree::domain::{Error, Id, Job, JobKind, JobState, Result};
use birchtree::persistence::redb::secrets::{SecretBox, SecretKey};
use birchtree::persistence::redb::schema;
use birchtree::persistence::redb::RedbStore;
use redb::{ReadableDatabase, ReadableTable};

use common::{open_store, temp_store, T0};

const NOW: u64 = T0 + 1_000;

fn keyring(id: u32, fill: u8) -> SecretBox {
    SecretBox::new(SecretKey::new(id, [fill; 32]), Vec::new()).expect("keyring")
}

fn signup(store: &RedbStore, username: &str, send_mail: bool) -> Id {
    let account = store
        .create_local_account(NewLocalAccount {
            username: username.to_string(),
            normalized_username: username.to_string(),
            domain: "example.social".to_string(),
            display_name: username.to_string(),
            password_hash: "old-hash".to_string(),
            email: Some(format!("{username}@example.com")),
            actor_uri: format!("https://example.social/users/{username}"),
            now_ms: T0,
            send_confirmation_email: send_mail,
            approved: true,
            created_by_application: None,
            locale: None,
            invite_request: None,
            invite_code: None,
            time_zone: None,
            date_of_birth: None,
        })
        .expect("create account");
    account.id
}

fn mail_jobs(store: &RedbStore, kind: JobKind) -> Vec<Job> {
    store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| job.kind == kind)
        .collect()
}

fn db_bytes(dir: &std::path::Path) -> Vec<u8> {
    std::fs::read(dir.join("birchtree.redb")).expect("read db file")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn token_keys(dir: &std::path::Path) -> Vec<String> {
    let db = redb::Database::open(dir.join("birchtree.redb")).expect("open raw db");
    let read = db.begin_read().expect("read");
    let table = read.open_table(schema::EMAIL_TOKENS).expect("table");
    let mut keys: Vec<String> = Vec::new();
    for row in table.iter().expect("iter") {
        let (key, _) = row.expect("row");
        keys.push(key.value().to_string());
    }
    keys
}

// -- 1.1 / 1.2 registration bearer -----------------------------------------

#[test]
fn registration_tokens_are_random_and_not_derived_from_job_ids() {
    let (_dir, store) = temp_store();
    signup(&store, "ada", true);
    signup(&store, "bob", true);
    let jobs: Vec<Job> = mail_jobs(&store, JobKind::SendConfirmationEmail);
    assert_eq!(jobs.len(), 2);
    let tokens: Vec<&str> = jobs
        .iter()
        .map(|job| job.payload["token"].as_str().expect("token"))
        .collect();
    assert_ne!(tokens[0], tokens[1]);
    for (job, token) in jobs.iter().zip(&tokens) {
        assert_eq!(token.len(), 64, "256 random bits as hex");
        let derived: String = crypto::sha256_hex(format!("confirmation:{}", job.id).as_bytes());
        assert_ne!(*token, derived, "job ids must not reproduce the bearer");
    }
}

#[test]
fn bearer_is_stored_as_digest_and_redeems_once() {
    let (dir, store) = temp_store();
    let account = signup(&store, "carol", true);
    let job: Job = mail_jobs(&store, JobKind::SendConfirmationEmail).remove(0);
    let token: String = job.payload["token"].as_str().expect("token").to_string();
    drop(store);

    let keys: Vec<String> = token_keys(dir.path());
    assert_eq!(keys.len(), 1);
    assert!(keys[0].starts_with("h1:"), "digest key, got {}", keys[0]);
    assert!(!keys[0].contains(&token), "plaintext bearer must not be a key");

    let store = open_store(dir.path());
    let user = confirm_email(&store, &token, NOW).expect("confirms");
    assert_eq!(user.account_id, account);
    assert!(user.email_verified);
    assert!(matches!(confirm_email(&store, &token, NOW), Err(Error::NotFound(_))));
}

#[test]
fn legacy_plaintext_token_rows_are_rekeyed_on_first_use() {
    let (dir, store) = temp_store();
    let account = signup(&store, "dave", false);
    drop(store);
    {
        let db = redb::Database::open(dir.path().join("birchtree.redb")).expect("raw");
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(schema::EMAIL_TOKENS).expect("table");
            let record = EmailToken {
                account_id: account,
                kind: EmailTokenKind::Confirmation,
                created_at_ms: T0,
            };
            table
                .insert("legacy-plaintext-bearer", serde_json::to_vec(&record).unwrap().as_slice())
                .expect("insert");
        }
        write.commit().expect("commit");
    }
    let store = open_store(dir.path());
    let user = confirm_email(&store, "legacy-plaintext-bearer", NOW).expect("legacy bearer lives");
    assert!(user.email_verified);
    drop(store);
    assert!(token_keys(dir.path()).is_empty(), "consumed");
}

#[test]
fn sealed_outbox_bearer_is_absent_from_the_database_file_and_recoverable() {
    let (dir, store) = temp_store();
    store.install_secret_box(keyring(1, 7));
    let account = signup(&store, "erin", true);
    let job: Job = mail_jobs(&store, JobKind::SendConfirmationEmail).remove(0);
    let stored: String = job.payload["token"].as_str().expect("token").to_string();
    assert!(stored.starts_with("bt1:"), "sealed, got {stored}");
    let token: String = store.reveal_mail_token(account, &stored).expect("reveal");
    assert_eq!(token.len(), 64);
    drop(store);
    let bytes: Vec<u8> = db_bytes(dir.path());
    assert!(!contains(&bytes, &token), "bearer must not appear in the file");

    // The sealed value is bound to its account.
    let store = open_store(dir.path());
    assert!(store.reveal_mail_token(Id::new(account.value() + 1), &stored).is_err());
    // Missing key fails closed.
    store.install_secret_box(SecretBox::disabled());
    assert!(store.reveal_mail_token(account, &stored).is_err());
}

struct CapturingMailer {
    sent: Mutex<Vec<(String, String)>>,
}

impl MailSender for CapturingMailer {
    fn send_confirmation(&self, to: &str, _account_id: Id, token: &str) -> Result<()> {
        self.sent.lock().unwrap().push((to.to_string(), token.to_string()));
        Ok(())
    }
    fn send_password_reset(&self, to: &str, _account_id: Id, token: &str) -> Result<()> {
        self.sent.lock().unwrap().push((to.to_string(), token.to_string()));
        Ok(())
    }
}

fn run_handler(store: &RedbStore, job: &Job, mailer: &CapturingMailer) -> Result<()> {
    let blobs = birchtree::media::blob_store::FsBlobStore::open(
        tempfile::tempdir().expect("blob dir").keep(),
    )
    .expect("blobs");
    birchtree::jobs::handlers::handle(job, store, mailer, &blobs, &NoStreaming)
}

#[test]
fn mail_job_sends_then_completion_leaves_no_bearer() {
    let (_dir, store) = temp_store();
    store.install_secret_box(keyring(2, 9));
    let account = signup(&store, "fay", true);
    let mailer = CapturingMailer { sent: Mutex::new(Vec::new()) };

    let claimed: Vec<Job> = store.claim_due(NOW, 16).expect("claim");
    let job: &Job = claimed
        .iter()
        .find(|job| job.kind == JobKind::SendConfirmationEmail)
        .expect("mail job");
    // A retry re-sends the same recoverable bearer.
    run_handler(&store, job, &mailer).expect("first send");
    run_handler(&store, job, &mailer).expect("retry send");
    let sent = mailer.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
    assert_eq!(sent[0].0, "fay@example.com");
    let token: String = sent[0].1.clone();

    store.complete(job.id).expect("complete");
    let done: Job = store.get(job.id).expect("get").expect("job");
    assert_eq!(done.state, JobState::Succeeded);
    assert!(done.payload.get("token").is_none());
    assert!(done.payload.get("email").is_none());
    assert_eq!(done.payload["account_id"], account.value());
    assert_eq!(done.payload["redacted"], true);

    // The mailed bearer still redeems.
    assert!(confirm_email(&*store, &token, NOW).expect("confirm").email_verified);
}

#[test]
fn mail_job_without_token_fails_visibly_and_quarantines_redacted() {
    let (_dir, store) = temp_store();
    let account = signup(&store, "gus", false);
    let mailer = CapturingMailer { sent: Mutex::new(Vec::new()) };
    let job = store
        .enqueue(
            JobKind::SendConfirmationEmail,
            serde_json::json!({"account_id": account.value(), "email": "gus@example.com"}),
            T0,
        )
        .expect("enqueue");
    let err = run_handler(&store, &job, &mailer).expect_err("no fallback token");
    assert!(err.to_string().contains("missing token"), "{err}");
    assert!(mailer.sent.lock().unwrap().is_empty());

    // Retry budget exhausted -> quarantined with the recipient redacted.
    let mut current: Job = job;
    for _ in 0..current.max_attempts {
        let claimed = store.claim_due(u64::MAX / 2, 4).expect("claim");
        assert!(!claimed.is_empty() || current.state == JobState::Quarantined);
        current = store.retry(current.id, "missing token", NOW).expect("retry");
    }
    assert_eq!(current.state, JobState::Quarantined);
    assert!(current.payload.get("email").is_none());
    assert_eq!(current.last_error.as_deref(), Some("missing token"));
}

#[test]
fn redaction_sweep_cleans_finished_legacy_rows_only() {
    let (dir, store) = temp_store();
    let pending = store
        .enqueue(
            JobKind::SendPasswordResetEmail,
            serde_json::json!({"account_id": 2, "email": "y@example.com", "token": "def"}),
            T0,
        )
        .expect("enqueue");
    drop(store);
    // A row finished by a build that did not redact.
    let mut legacy = Job::new(
        Id::new(9_000_001),
        JobKind::SendPasswordResetEmail,
        serde_json::json!({"account_id": 1, "email": "x@example.com", "token": "abc"}),
        T0,
        T0,
    );
    legacy.state = JobState::Succeeded;
    {
        let db = redb::Database::open(dir.path().join("birchtree.redb")).expect("raw");
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(schema::JOBS).expect("table");
            table
                .insert(legacy.id.value(), serde_json::to_vec(&legacy).unwrap().as_slice())
                .unwrap();
        }
        write.commit().expect("commit");
    }
    let store = open_store(dir.path());
    assert_eq!(store.redact_finished_job_payloads(10).expect("sweep"), 1);
    assert_eq!(store.redact_finished_job_payloads(10).expect("again"), 0);
    let cleaned: Job = store.get(legacy.id).expect("get").expect("job");
    assert!(cleaned.payload.get("token").is_none());
    let untouched: Job = store.get(pending.id).expect("get").expect("job");
    assert_eq!(untouched.payload["token"], "def", "pending jobs keep their bearer");
}

// -- 1.4 atomic redemption --------------------------------------------------

#[test]
fn failed_state_change_does_not_consume_the_token() {
    let (_dir, store) = temp_store();
    // A bearer whose account row is gone: the state change cannot apply.
    store
        .store_email_token(
            "orphan-bearer",
            EmailToken {
                account_id: Id::new(424_242),
                kind: EmailTokenKind::Confirmation,
                created_at_ms: T0,
            },
        )
        .expect("store");
    assert!(matches!(confirm_email(&*store, "orphan-bearer", NOW), Err(Error::NotFound(_))));
    assert!(
        store.redeem_email_token("orphan-bearer").expect("redeem").is_some(),
        "the bearer survives a failed update"
    );

    store
        .store_email_token(
            "orphan-reset",
            EmailToken {
                account_id: Id::new(424_243),
                kind: EmailTokenKind::PasswordReset,
                created_at_ms: T0,
            },
        )
        .expect("store");
    assert!(reset_password(&*store, "orphan-reset", "hash".into(), NOW, 3_600_000).is_err());
    assert!(store.redeem_email_token("orphan-reset").expect("redeem").is_some());
}

#[test]
fn concurrent_redemptions_yield_one_winner() {
    let (_dir, store) = temp_store();
    let account = signup(&store, "hana", false);
    store
        .store_email_token(
            "race-confirm",
            EmailToken { account_id: account, kind: EmailTokenKind::Confirmation, created_at_ms: T0 },
        )
        .expect("store");
    store
        .store_email_token(
            "race-reset",
            EmailToken { account_id: account, kind: EmailTokenKind::PasswordReset, created_at_ms: T0 },
        )
        .expect("store");

    let mut handles = Vec::new();
    for index in 0..8 {
        let store: Arc<RedbStore> = store.clone();
        handles.push(std::thread::spawn(move || {
            let confirmed: bool = confirm_email(&*store, "race-confirm", NOW).is_ok();
            let reset: bool =
                reset_password(&*store, "race-reset", format!("hash-{index}"), NOW, 3_600_000).is_ok();
            (confirmed, reset)
        }));
    }
    let results: Vec<(bool, bool)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.0).count(), 1);
    assert_eq!(results.iter().filter(|r| r.1).count(), 1);
}

#[test]
fn reset_rejects_expired_used_unknown_and_wrong_kind_alike() {
    let (_dir, store) = temp_store();
    let account = signup(&store, "ivan", false);
    let ttl: u64 = 6 * 3_600_000;
    for (name, kind, created) in [
        ("expired", EmailTokenKind::PasswordReset, T0),
        ("confirm-kind", EmailTokenKind::Confirmation, NOW),
    ] {
        store
            .store_email_token(
                name,
                EmailToken { account_id: account, kind, created_at_ms: created },
            )
            .expect("store");
    }
    let late: u64 = T0 + ttl + 1;
    let expired = reset_password(&*store, "expired", "new".into(), late, ttl).expect_err("expired");
    let wrong = reset_password(&*store, "confirm-kind", "new".into(), NOW, ttl).expect_err("kind");
    let unknown = reset_password(&*store, "nope", "new".into(), NOW, ttl).expect_err("unknown");
    let used = reset_password(&*store, "expired", "new".into(), NOW, ttl).expect_err("used");
    for err in [&expired, &wrong, &unknown, &used] {
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }
    assert_eq!(expired.to_string(), unknown.to_string());
    assert_eq!(used.to_string(), unknown.to_string());
    // The old password is untouched.
    let user = store.user_by_account_id(account).expect("user").expect("row");
    assert_eq!(user.password_hash, "old-hash");

    store
        .store_email_token(
            "fresh",
            EmailToken { account_id: account, kind: EmailTokenKind::PasswordReset, created_at_ms: NOW },
        )
        .expect("store");
    let updated = reset_password(&*store, "fresh", "new-hash".into(), NOW + 1, ttl).expect("reset");
    assert_eq!(updated.password_hash, "new-hash");
}

#[test]
fn issue_email_token_stores_digest_and_queues_in_one_commit() {
    let (_dir, store) = temp_store();
    let account = signup(&store, "jade", false);
    store
        .issue_email_token(account, "jade@example.com", EmailTokenKind::PasswordReset, NOW)
        .expect("issue");
    let job: Job = mail_jobs(&store, JobKind::SendPasswordResetEmail).remove(0);
    let token: String = job.payload["token"].as_str().expect("token").to_string();
    let updated = reset_password(&*store, &token, "after".into(), NOW, 3_600_000).expect("redeem");
    assert_eq!(updated.password_hash, "after");
}

#[test]
fn prune_removes_stale_rows_in_bounded_batches() {
    let (dir, store) = temp_store();
    let account = signup(&store, "kyle", false);
    let ttl: u64 = 3_600_000;
    for index in 0..5 {
        store
            .store_email_token(
                &format!("old-reset-{index}"),
                EmailToken { account_id: account, kind: EmailTokenKind::PasswordReset, created_at_ms: T0 },
            )
            .expect("store");
    }
    store
        .store_email_token(
            "fresh-reset",
            EmailToken { account_id: account, kind: EmailTokenKind::PasswordReset, created_at_ms: T0 + 10 * ttl },
        )
        .expect("store");
    let now: u64 = T0 + 10 * ttl;
    assert_eq!(store.prune_email_secrets(now, ttl, 3).expect("prune"), 3);
    assert_eq!(store.prune_email_secrets(now, ttl, 100).expect("prune"), 2);
    assert_eq!(store.prune_email_secrets(now, ttl, 100).expect("prune"), 0);
    drop(store);
    assert_eq!(token_keys(dir.path()).len(), 1, "only the fresh bearer remains");
}

// -- 1.3 sealed TOTP --------------------------------------------------------

fn totp(account_id: Id, secret: &str) -> TotpRecord {
    TotpRecord {
        account_id,
        secret_base32: secret.to_string(),
        created_at_ms: T0,
        confirmed: true,
        backup_hashes: vec![],
        last_counter: 0,
    }
}

const SEED: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

#[test]
fn totp_seed_is_sealed_and_fails_closed_without_the_key() {
    let (dir, store) = temp_store();
    store.install_secret_box(keyring(5, 3));
    let account = Id::new(77);
    store.put_totp(totp(account, SEED)).expect("put");
    assert_eq!(store.totp_for(account).expect("get").expect("row").secret_base32, SEED);

    // Missing key: an error, not "no enrollment".
    store.install_secret_box(SecretBox::disabled());
    assert!(store.totp_for(account).is_err());
    // Wrong key: authentication failure.
    store.install_secret_box(keyring(5, 4));
    assert!(store.totp_for(account).is_err());
    store.install_secret_box(keyring(5, 3));
    assert!(store.totp_for(account).expect("get").is_some());
    drop(store);
    assert!(!contains(&db_bytes(dir.path()), SEED), "seed must not be in the file");
}

#[test]
fn tampered_totp_ciphertext_and_moved_rows_are_rejected() {
    let (dir, store) = temp_store();
    store.install_secret_box(keyring(5, 3));
    store.put_totp(totp(Id::new(1), SEED)).expect("put");
    store.put_totp(totp(Id::new(2), SEED)).expect("put");
    drop(store);

    {
        let db = redb::Database::open(dir.path().join("birchtree.redb")).expect("raw");
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(schema::TOTP_SECRETS).expect("table");
            let mut one: Vec<u8> = table.get(1u64).unwrap().unwrap().value().to_vec();
            let last: usize = one.len() - 1;
            one[last] ^= 0x01;
            table.insert(1u64, one.as_slice()).unwrap();
            // Row 2's ciphertext copied over row 3's key (a swap attack).
            let two: Vec<u8> = table.get(2u64).unwrap().unwrap().value().to_vec();
            table.insert(3u64, two.as_slice()).unwrap();
        }
        write.commit().expect("commit");
    }
    let store = open_store(dir.path());
    store.install_secret_box(keyring(5, 3));
    assert!(store.totp_for(Id::new(1)).is_err(), "tampered");
    assert!(store.totp_for(Id::new(3)).is_err(), "context-bound");
    assert!(store.totp_for(Id::new(2)).expect("intact").is_some());
}

#[test]
fn legacy_plaintext_totp_rows_migrate_and_keys_rotate() {
    let (dir, store) = temp_store();
    let account = Id::new(9);
    store.put_totp(totp(account, SEED)).expect("plaintext put");
    assert!(store.seal_legacy_secrets().is_err(), "no key, no migration");

    store.install_secret_box(keyring(1, 1));
    assert_eq!(store.seal_legacy_secrets().expect("seal").totp_rows, 1);
    assert_eq!(store.seal_legacy_secrets().expect("repeat").totp_rows, 0);

    // Rotate: new active key 2, old key 1 retired.
    let rotated = SecretBox::new(
        SecretKey::new(2, [2; 32]),
        vec![SecretKey::new(1, [1; 32])],
    )
    .expect("keyring");
    store.install_secret_box(rotated);
    assert!(store.totp_for(account).expect("opens with retired key").is_some());
    assert_eq!(store.seal_legacy_secrets().expect("reseal").totp_rows, 1);
    // Retired key 1 can now be dropped.
    store.install_secret_box(keyring(2, 2));
    assert!(store.totp_for(account).expect("opens").is_some());
    drop(store);
    assert!(!contains(&db_bytes(dir.path()), SEED));
}

// -- 11 WebAuthn index ------------------------------------------------------

fn credential(account: u64, index: u32) -> WebauthnCredential {
    WebauthnCredential {
        account_id: Id::new(account),
        credential_id: format!("cred-{account}-{index}").into_bytes(),
        public_key_uncompressed: vec![4; 65],
        sign_count: 0,
        created_at_ms: T0,
    }
}

#[test]
fn webauthn_lookup_uses_account_index_and_isolates_accounts() {
    let (_dir, store) = temp_store();
    for account in [5u64, 6, 7] {
        for index in 0..3 {
            store.put_webauthn(credential(account, index)).expect("put");
        }
    }
    let five = store.webauthn_for_account(Id::new(5)).expect("list");
    assert_eq!(five.len(), 3);
    assert!(five.iter().all(|c| c.account_id == Id::new(5)));
    assert!(store.webauthn_for_account(Id::new(8)).expect("list").is_empty());
    assert!(store.webauthn_for_account(Id::new(u64::MAX)).expect("list").is_empty());

    assert!(store.delete_webauthn(Id::new(5), b"cred-5-1").expect("delete"));
    assert!(!store.delete_webauthn(Id::new(6), b"cred-5-2").expect("not owner"));
    let five = store.webauthn_for_account(Id::new(5)).expect("list");
    assert_eq!(five.len(), 2);
    assert_eq!(store.webauthn_for_account(Id::new(6)).expect("list").len(), 3);
}

#[test]
fn webauthn_limit_is_enforced_atomically_not_by_scan_cap() {
    let (_dir, store) = temp_store();
    let limit: u32 = MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT as u32;
    for index in 0..limit {
        store.put_webauthn(credential(11, index)).expect("within limit");
    }
    let over = store.put_webauthn(credential(11, limit)).expect_err("over limit");
    assert!(matches!(over, Error::Conflict(_)), "{over:?}");
    assert_eq!(
        store.webauthn_for_account(Id::new(11)).expect("list").len(),
        MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT,
        "no credential is silently dropped from the listing"
    );
    // A sign-count update of an existing credential is not a registration.
    let mut bumped = credential(11, 0);
    bumped.sign_count = 9;
    store.put_webauthn(bumped).expect("update at the limit");
    // Another account is unaffected, and a freed slot is reusable.
    store.put_webauthn(credential(12, 0)).expect("other account");
    assert!(store.delete_webauthn(Id::new(11), b"cred-11-3").expect("delete"));
    store.put_webauthn(credential(11, limit)).expect("slot freed");
    // A credential id cannot be claimed by a second account.
    let mut stolen = credential(12, 1);
    stolen.credential_id = b"cred-11-0".to_vec();
    assert!(matches!(store.put_webauthn(stolen), Err(Error::Conflict(_))));
}

#[test]
fn webauthn_index_is_backfilled_for_pre_index_databases() {
    let (dir, store) = temp_store();
    drop(store);
    {
        // Rows written by a build that had no index (and no marker).
        let db = redb::Database::open(dir.path().join("birchtree.redb")).expect("raw");
        let write = db.begin_write().expect("write");
        {
            let mut table = write.open_table(schema::WEBAUTHN_CREDS).expect("table");
            for account in [21u64, 22] {
                for index in 0..4 {
                    let cred = credential(account, index);
                    table
                        .insert(
                            cred.credential_id.as_slice(),
                            serde_json::to_vec(&cred).unwrap().as_slice(),
                        )
                        .unwrap();
                }
            }
        }
        write.commit().expect("commit");
    }
    let store = open_store(dir.path());
    assert_eq!(store.webauthn_for_account(Id::new(21)).expect("list").len(), 4);
    assert_eq!(store.webauthn_for_account(Id::new(22)).expect("list").len(), 4);
    // Mutations after the backfill keep the index consistent.
    store.put_webauthn(credential(21, 9)).expect("put");
    assert_eq!(store.webauthn_for_account(Id::new(21)).expect("list").len(), 5);
    drop(store);
    let store = open_store(dir.path());
    assert_eq!(store.webauthn_for_account(Id::new(21)).expect("list").len(), 5);
}

#[test]
fn reissue_supersedes_older_bearers_of_the_same_kind() {
    let (_dir, store) = temp_store();
    let account = signup(&store, "lena", false);
    store
        .issue_email_token(account, "lena@example.com", EmailTokenKind::PasswordReset, NOW)
        .expect("first issue");
    let first: String = mail_jobs(&store, JobKind::SendPasswordResetEmail)
        .remove(0)
        .payload["token"]
        .as_str()
        .expect("token")
        .to_string();
    store
        .issue_email_token(account, "lena@example.com", EmailTokenKind::PasswordReset, NOW + 1)
        .expect("reissue");
    let jobs = mail_jobs(&store, JobKind::SendPasswordResetEmail);
    assert_eq!(jobs.len(), 2);
    let second: String = jobs[1].payload["token"].as_str().expect("token").to_string();
    assert_ne!(first, second);
    // The superseded bearer is dead; the replacement redeems.
    assert!(matches!(
        reset_password(&*store, &first, "nope".into(), NOW + 2, 3_600_000),
        Err(Error::NotFound(_))
    ));
    let updated =
        reset_password(&*store, &second, "new-hash".into(), NOW + 2, 3_600_000).expect("redeem");
    assert_eq!(updated.password_hash, "new-hash");
}
