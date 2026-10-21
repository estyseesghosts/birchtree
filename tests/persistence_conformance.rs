//! Persistence conformance suite (docs/persistence-conformance.md).
//!
//! Runs against the real `redb` adapter (no mocks). Covers unique indexes,
//! counter/index consistency, transaction atomicity, migrations, dedup and job
//! lease/retry/quarantine, and read-after-commit.

mod common;

use std::sync::Arc;

use birchtree::application::auth;
use birchtree::application::ports::{
    AccountStore, JobQueue, NewNotification, NewPollSpec, NewQuoteSpec, NewStatus,
    NotificationStore, OAuthStore, PollStore, QuoteStore, RelationshipStore, SessionStore,
    StatusStore,
};
use birchtree::application::crypto;
use birchtree::domain::{
    Id, JobKind, JobState, LocalUser, NotificationType, QuotePolicy, QuoteState, Visibility,
};
use birchtree::domain::job::LEASE_MS;
use birchtree::jobs::handlers;
use birchtree::mail::LoggingMailSender;

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
        false,
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
            time_zone: None,
            date_of_birth: None,
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

// ---------------------------------------------------------------------------
// Phase 3: status/interaction atomicity, idempotency and durability.
// ---------------------------------------------------------------------------

/// A minimal public status input for direct `StatusStore::create_status` use.
fn new_status(account_id: Id, text: &str) -> NewStatus {
    NewStatus {
        remote: None,
        account_id,
        uri_base: "https://example.social/users/test/statuses".to_string(),
        url_base: "https://example.social/@test".to_string(),
        source_text: text.to_string(),
        content_html: format!("<p>{text}</p>"),
        visibility: Visibility::Public,
        ..Default::default()
    }
}

#[test]
fn status_create_commits_notification_and_poll_jobs_atomically() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let carol = mk_account(&store, "carol");
    let now = T0 + 1_000;

    // alice's parent status, quoted by the reply below.
    let parent = store
        .create_status(new_status(alice.id, "root post"), now)
        .unwrap()
        .status;

    // ONE transaction: reply + mention + poll + quote all commit together.
    let mut reply = new_status(bob.id, "@carol reply with poll and quote");
    reply.in_reply_to_id = Some(parent.id);
    reply.in_reply_to_account_id = Some(parent.account_id);
    reply.mentions = vec![carol.id];
    reply.poll = Some(NewPollSpec {
        multiple: false,
        hide_totals: false,
        expires_at_ms: now + 3_600_000,
        options: vec!["tea".to_string(), "coffee".to_string()],
    });
    reply.quote = Some(NewQuoteSpec {
        quoted_status_id: parent.id,
        quoted_account_id: parent.account_id,
        approval_policy: QuotePolicy::Public,
    });
    let created = store.create_status(reply, now + 1).unwrap();
    assert!(!created.replayed);
    let reply_id = created.status.id;

    // Read-after-commit: every mutation of the transaction is visible.
    let row = store.status_by_id(reply_id).unwrap().unwrap();
    assert_eq!(row.account_id, bob.id);
    assert_eq!(row.replies_count, 0);

    let parent_row = store.status_by_id(parent.id).unwrap().unwrap();
    assert_eq!(parent_row.replies_count, 1, "parent counter bumped");

    let poll = store.poll_by_status(reply_id).unwrap().unwrap();
    assert_eq!(poll.status_id, reply_id);
    assert_eq!(poll.options.len(), 2);
    assert_eq!(store.poll_by_id(poll.id).unwrap().unwrap().id, poll.id);

    let quote = store.quote_by_quoting_status(reply_id).unwrap().unwrap();
    assert_eq!(quote.state, QuoteState::Accepted);
    assert_eq!(quote.quoted_status_id, parent.id);
    assert_eq!(
        store.status_by_id(parent.id).unwrap().unwrap().quotes_count,
        1,
        "quoted counter bumped"
    );

    let stats = store.account_stats(bob.id).unwrap();
    assert_eq!(stats.statuses_count, 1);
    assert_eq!(stats.last_status_at_ms, Some(now + 1));

    // The follow-up jobs committed WITH the mutation: one PollExpired and one
    // FanOutNotification per (reply parent, mention, quote) — exactly the
    // expected kinds, no more.
    let jobs = store.all_jobs().unwrap();
    let follow_ups: Vec<JobKind> = jobs
        .iter()
        .map(|j| j.kind)
        .filter(|k| *k != JobKind::UserCreated)
        .collect();
    assert_eq!(
        follow_ups
            .iter()
            .filter(|k| **k == JobKind::PollExpired)
            .count(),
        1
    );
    assert_eq!(
        follow_ups
            .iter()
            .filter(|k| **k == JobKind::FanOutNotification)
            .count(),
        3,
        "parent-mention + explicit mention + quote"
    );
    let poll_job = jobs
        .iter()
        .find(|j| j.kind == JobKind::PollExpired)
        .unwrap();
    assert_eq!(
        poll_job.payload.get("poll_id").unwrap().as_u64().unwrap(),
        poll.id.value()
    );

    // A BAD input aborts the whole transaction: no status row AND no jobs.
    let jobs_before = store.all_jobs().unwrap().len();
    let mut bad = new_status(bob.id, "media not found");
    bad.media_ids = vec![Id::new(424_242)];
    assert!(store.create_status(bad, now + 2).is_err());
    assert_eq!(
        store.statuses_by_account(
            bob.id,
            birchtree::application::ports::AccountStatusesQuery {
                max_id: None,
                since_id: None,
                min_id: None,
                limit: 20,
                pinned_only: false,
                exclude_replies: false,
                exclude_reblogs: false,
            }
        )
        .unwrap()
        .len(),
        1,
        "aborted create leaves no status row"
    );
    assert_eq!(store.all_jobs().unwrap().len(), jobs_before, "no jobs either");
    assert_eq!(store.account_stats(bob.id).unwrap().statuses_count, 1);
}

#[test]
fn fan_out_notification_job_is_idempotent() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let status = store
        .create_status(new_status(alice.id, "favourite me"), T0)
        .unwrap()
        .status;

    let payload = birchtree::application::notifications::fan_out_payload(
        alice.id,
        NotificationType::Favourite,
        Some(bob.id),
        Some(status.id),
        T0 + 5,
    );
    let job = birchtree::domain::Job::new(
        Id::new(9_999),
        JobKind::FanOutNotification,
        payload,
        T0 + 5,
        T0 + 5,
    );
    let mail = LoggingMailSender::default();
    let blob_dir = tempfile::tempdir().unwrap();
    let blobs = birchtree::media::blob_store::FsBlobStore::open(blob_dir.path()).unwrap();

    // Running the SAME job twice (a retried lease) creates one row: the
    // dedup index refuses the second insert.
    handlers::handle(&job, &*store, &mail, &blobs, &birchtree::application::ports::NoStreaming).unwrap();
    handlers::handle(&job, &*store, &mail, &blobs, &birchtree::application::ports::NoStreaming).unwrap();

    let probe = store
        .create_notification(NewNotification {
            recipient_id: alice.id,
            r#type: NotificationType::Favourite,
            actor_id: Some(bob.id),
            status_id: Some(status.id),
            created_at_ms: T0 + 5,
            filtered: false,
        })
        .unwrap();
    assert!(probe.is_none(), "retried fan-out must not duplicate a row");

    // A NEW interaction (different created_at_ms) notifies again.
    let fresh = store
        .create_notification(NewNotification {
            recipient_id: alice.id,
            r#type: NotificationType::Favourite,
            actor_id: Some(bob.id),
            status_id: Some(status.id),
            created_at_ms: T0 + 6,
            filtered: false,
        })
        .unwrap();
    assert!(fresh.is_some(), "a fresh interaction must create a row");
}

#[test]
fn interaction_replay_does_not_duplicate_rows_or_jobs() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let status = store
        .create_status(new_status(alice.id, "interact with me"), T0)
        .unwrap()
        .status;

    let fan_out_jobs = |store: &birchtree::persistence::redb::RedbStore| {
        store
            .all_jobs()
            .unwrap()
            .into_iter()
            .filter(|j| j.kind == JobKind::FanOutNotification)
            .collect::<Vec<_>>()
    };

    // Favourite twice: one row, count 1, exactly one fan-out job.
    store.favourite_status(bob.id, status.id, T0 + 10).unwrap();
    store.favourite_status(bob.id, status.id, T0 + 11).unwrap();
    assert_eq!(store.favourited_by(status.id, None, 10).unwrap().len(), 1);
    assert_eq!(
        store.status_by_id(status.id).unwrap().unwrap().favourites_count,
        1
    );
    assert_eq!(fan_out_jobs(&store).len(), 1, "idempotent replay adds no job");

    // Unfavourite + favourite again: a FRESH interaction notifies again, but
    // the row stays unique (pair-keyed) and the counter never over-counts.
    store.unfavourite_status(bob.id, status.id, T0 + 20).unwrap();
    store.favourite_status(bob.id, status.id, T0 + 30).unwrap();
    assert_eq!(store.favourited_by(status.id, None, 10).unwrap().len(), 1);
    assert_eq!(
        store.status_by_id(status.id).unwrap().unwrap().favourites_count,
        1
    );
    let jobs = fan_out_jobs(&store);
    assert_eq!(jobs.len(), 2, "fresh interaction enqueues a second job");
    let created: Vec<u64> = jobs
        .iter()
        .map(|j| j.payload.get("created_at_ms").unwrap().as_u64().unwrap())
        .collect();
    assert_eq!(created, vec![T0 + 10, T0 + 30]);
}

#[test]
fn status_idempotency_record_replays_within_hour() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let now = T0 + 1_000;

    let make = |text: &str, now: u64| {
        let mut input = new_status(alice.id, text);
        input.mentions = vec![bob.id];
        input.idempotency_key = Some("retry-42".to_string());
        store.create_status(input, now).unwrap()
    };

    let first = make("first text", now);
    assert!(!first.replayed);

    // A replay with the SAME key returns the ORIGINAL status, one hour later.
    let replay = make("different text", now + 3_599_000);
    assert!(replay.replayed);
    assert_eq!(replay.status.id, first.status.id);
    assert_eq!(replay.status.source_text, "first text");

    // Only ONE set of jobs exists (the replay enqueues nothing) and the
    // author's counters were bumped exactly once.
    let fan_outs = store
        .all_jobs()
        .unwrap()
        .into_iter()
        .filter(|j| j.kind == JobKind::FanOutNotification)
        .count();
    assert_eq!(fan_outs, 1, "replay must not duplicate follow-up jobs");
    let stats = store.account_stats(alice.id).unwrap();
    assert_eq!(stats.statuses_count, 1);
    assert_eq!(stats.last_status_at_ms, Some(now));
}

#[test]
fn interaction_and_job_survive_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let now = T0 + 100;

    // Commit an interaction (and its fan-out job), then "crash" before the
    // worker ever ran by dropping the store.
    let (alice_id, bob_id, status_id) = {
        let store = open_store(dir.path());
        let alice = mk_account(&store, "alice");
        let bob = mk_account(&store, "bob");
        let status = store
            .create_status(new_status(alice.id, "durable post"), now)
            .unwrap()
            .status;
        store.favourite_status(bob.id, status.id, now + 10).unwrap();
        (alice.id, bob.id, status.id)
    };

    // Reopen: the status row, the favourite row, the counter and the PENDING
    // job all survived.
    let store = open_store(dir.path());
    let status = store.status_by_id(status_id).unwrap().unwrap();
    assert_eq!(status.favourites_count, 1);
    assert_eq!(store.favourited_by(status_id, None, 10).unwrap().len(), 1);
    let pending = store
        .all_jobs()
        .unwrap()
        .into_iter()
        .filter(|j| j.kind == JobKind::FanOutNotification)
        .collect::<Vec<_>>();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, JobState::Pending);

    // Simulate the worker: claim, run, complete — and run the handler a
    // SECOND time (a crash between handler and completion would replay it).
    let claimed = store.claim_due(now + 20, 10).unwrap();
    let job = claimed
        .into_iter()
        .find(|j| j.kind == JobKind::FanOutNotification)
        .expect("fan-out job claimable");
    let mail = LoggingMailSender::default();
    let blob_dir = tempfile::tempdir().unwrap();
    let blobs = birchtree::media::blob_store::FsBlobStore::open(blob_dir.path()).unwrap();
    handlers::handle(&job, &store, &mail, &blobs, &birchtree::application::ports::NoStreaming).unwrap();
    store.complete(job.id).unwrap();
    handlers::handle(&job, &store, &mail, &blobs, &birchtree::application::ports::NoStreaming).unwrap();
    assert_eq!(store.get(job.id).unwrap().unwrap().state, JobState::Succeeded);

    // Exactly ONE notification row exists after running the handler twice.
    let probe = store
        .create_notification(NewNotification {
            recipient_id: alice_id,
            r#type: NotificationType::Favourite,
            actor_id: Some(bob_id),
            status_id: Some(status_id),
            created_at_ms: now + 10,
            filtered: false,
        })
        .unwrap();
    assert!(probe.is_none(), "replayed handler must not duplicate a row");
}
