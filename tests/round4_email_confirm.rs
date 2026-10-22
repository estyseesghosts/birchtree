//! D12 remainder: durable invites, email index, confirmation redemption,
//! signup-app ownership, public forgot-password.
//!
//! Covers: invite redb round-trip (create/redeem/expired/exhausted/
//! collision), email-index reads, redemption happy path + single-use +
//! wrong-kind + unknown, ownership grandfather/mismatch, public reset 200 +
//! job queued for known / nothing for unknown, and resend token-store
//! ordering (token row exists before/with the queued job).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;

use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{
    CreateAccountCommand, CreateInviteCommand, InviteStore, RegisterAccountCommand,
    check_confirmation_ownership, confirm_email, create_account, create_invite, register_account,
};
use birchtree::application::auth;
use birchtree::application::context::Context;
use birchtree::application::ports::{AccountStore, JobQueue, NewAuthCode, NewToken, OAuthStore};
use birchtree::domain::user::{EmailToken, EmailTokenKind, Invite};
use birchtree::domain::{
    AccessToken, AuthorizationCode, Clock, Error, Id, IdGenerator, JobKind, OAuthApplication,
    SystemClock, TimestampIdGenerator,
};
use common::{
    T0, TEST_PUBLIC_URL, TestServer, federation_config, form_request, get_request, send,
    server_info,
};

const BASE_URL: &str = "https://example.social";

fn open_store() -> (tempfile::TempDir, birchtree::persistence::redb::RedbStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = birchtree::persistence::redb::RedbStore::open(
        dir.path().join("birchtree.redb"),
        Arc::new(SystemClock),
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    )
    .expect("open store");
    (dir, store)
}

/// Minimal `OAuthStore` fake: only token issuance is implemented (the only
/// OAuth call the registration path makes).
#[derive(Default)]
struct FakeOAuth {
    next_id: Mutex<u64>,
    #[allow(dead_code)]
    tokens: Mutex<HashMap<Id, AccessToken>>,
}

impl OAuthStore for FakeOAuth {
    fn register_app(
        &self,
        _input: birchtree::application::ports::NewApp,
    ) -> Result<OAuthApplication, Error> {
        unimplemented!()
    }
    fn app_by_client_id(&self, _client_id: &str) -> Result<Option<OAuthApplication>, Error> {
        unimplemented!()
    }
    fn app_by_id(&self, _id: Id) -> Result<Option<OAuthApplication>, Error> {
        unimplemented!()
    }
    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken, Error> {
        let mut next = self.next_id.lock().unwrap();
        *next += 1;
        let token = AccessToken {
            id: Id::new(*next),
            app_id: input.app_id,
            user_id: input.user_id,
            token_hash: input.token_hash,
            scopes: input.scopes,
            created_at_ms: input.now_ms,
            expires_at_ms: input.expires_at_ms,
            revoked: false,
        };
        self.tokens.lock().unwrap().insert(token.id, token.clone());
        Ok(token)
    }
    fn token_by_plaintext(&self, _plaintext: &str) -> Result<Option<AccessToken>, Error> {
        unimplemented!()
    }
    fn revoke_token(&self, _plaintext: &str) -> Result<bool, Error> {
        unimplemented!()
    }
    fn revoke_account_tokens(&self, _account_id: Id) -> Result<Vec<Id>, Error> {
        unimplemented!()
    }
    fn create_authorization_code(&self, _input: NewAuthCode) -> Result<AuthorizationCode, Error> {
        unimplemented!()
    }
    fn authorization_code_by_plaintext(
        &self,
        _plaintext: &str,
    ) -> Result<Option<AuthorizationCode>, Error> {
        unimplemented!()
    }
    fn redeem_authorization_code(
        &self,
        _plaintext: &str,
        _token: NewToken,
        _now_ms: u64,
    ) -> Result<AccessToken, Error> {
        unimplemented!()
    }
}

fn test_app(id: u64) -> OAuthApplication {
    OAuthApplication {
        id: Id::new(id),
        name: "test app".to_string(),
        client_id: format!("client-{id}"),
        client_secret_hash: None,
        redirect_uris: vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        scopes: vec!["read".to_string(), "write".to_string()],
        website: None,
        created_at_ms: T0,
    }
}

fn invite_cmd(max_uses: Option<u32>, expires_in_ms: Option<u64>) -> CreateInviteCommand {
    CreateInviteCommand {
        created_by_account_id: Id::new(1),
        created_by_staff: false,
        comment: None,
        max_uses,
        expires_in_ms,
    }
}

fn mk_user(
    store: &birchtree::persistence::redb::RedbStore,
    username: &str,
    email: Option<&str>,
) -> birchtree::domain::Account {
    create_account(
        store,
        BASE_URL,
        T0,
        CreateAccountCommand {
            username: username.to_string(),
            email: email.map(str::to_string),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
}

// ---------------------------------------------------------------------------
// Durable invites (redb)
// ---------------------------------------------------------------------------

#[test]
fn invite_redb_roundtrip() {
    let (_dir, store) = open_store();
    let invite = create_invite(&store, T0, invite_cmd(None, None)).expect("issue");
    assert_eq!(invite.uses, 0);

    let read = store
        .invite_by_code(&invite.code)
        .expect("read")
        .expect("found");
    assert_eq!(read.code, invite.code);

    let redeemed = store.redeem_invite(&invite.code, T0).expect("redeem");
    assert_eq!(redeemed.uses, 1);
    let read = store
        .invite_by_code(&invite.code)
        .expect("read")
        .expect("found");
    assert_eq!(read.uses, 1);

    let err = store
        .redeem_invite("ZZZZZZZZ", T0)
        .expect_err("unknown code");
    assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
    assert!(store.invite_by_code("ZZZZZZZZ").expect("read").is_none());
}

#[test]
fn invite_redb_expired() {
    let (_dir, store) = open_store();
    let invite = create_invite(&store, T0, invite_cmd(None, Some(1_000))).expect("issue");
    assert!(store.redeem_invite(&invite.code, T0 + 999).is_ok());
    let err = store
        .redeem_invite(&invite.code, T0 + 1_000)
        .expect_err("expired");
    assert!(matches!(err, Error::Unauthorized(_)), "got {err:?}");
    // A refused redemption does not consume a use.
    let read = store
        .invite_by_code(&invite.code)
        .expect("read")
        .expect("found");
    assert_eq!(read.uses, 1);
}

#[test]
fn invite_redb_exhausted() {
    let (_dir, store) = open_store();
    let invite = create_invite(&store, T0, invite_cmd(Some(1), None)).expect("issue");
    assert!(store.redeem_invite(&invite.code, T0).is_ok());
    let err = store
        .redeem_invite(&invite.code, T0)
        .expect_err("exhausted");
    assert!(matches!(err, Error::Unauthorized(_)), "got {err:?}");
}

#[test]
fn invite_redb_collision() {
    let (_dir, store) = open_store();
    let invite = Invite {
        id: Id::new(4242),
        code: "abcdkmnp".to_string(),
        created_by_account_id: Id::new(1),
        created_by_staff: false,
        comment: None,
        max_uses: None,
        uses: 0,
        expires_at_ms: None,
        created_at_ms: T0,
    };
    store.create_invite(invite.clone()).expect("first insert");
    let err = store.create_invite(invite).expect_err("collision");
    assert!(matches!(err, Error::Conflict(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Email index
// ---------------------------------------------------------------------------

#[test]
fn email_index_read() {
    let (_dir, store) = open_store();
    mk_user(&store, "alice", Some("Alice@Example.COM"));
    // Normalized read, both cases.
    let found = store
        .account_by_email("alice@example.com")
        .expect("read")
        .expect("found");
    assert_eq!(found.username, "alice");
    let found = store
        .account_by_email("ALICE@EXAMPLE.COM")
        .expect("read")
        .expect("found");
    assert_eq!(found.username, "alice");
    // Unknown and empty: None, never an error.
    assert!(
        store
            .account_by_email("nobody@example.com")
            .expect("read")
            .is_none()
    );
    assert!(store.account_by_email("").expect("read").is_none());
    assert!(store.account_by_email("   ").expect("read").is_none());
}

// ---------------------------------------------------------------------------
// Redemption
// ---------------------------------------------------------------------------

fn store_token(
    store: &birchtree::persistence::redb::RedbStore,
    token: &str,
    account_id: Id,
    kind: EmailTokenKind,
) {
    store
        .store_email_token(
            token,
            EmailToken {
                account_id,
                kind,
                created_at_ms: T0,
            },
        )
        .expect("store token");
}

#[test]
fn confirm_email_happy_path_single_use() {
    let (_dir, store) = open_store();
    let account = mk_user(&store, "bob", Some("bob@example.com"));
    assert!(
        !store
            .user_by_account_id(account.id)
            .expect("user")
            .expect("row")
            .email_verified
    );

    store_token(
        &store,
        "tok-confirm-1",
        account.id,
        EmailTokenKind::Confirmation,
    );
    let user = confirm_email(&store, "tok-confirm-1", T0).expect("confirm");
    assert!(user.email_verified);
    assert!(
        store
            .user_by_account_id(account.id)
            .expect("user")
            .expect("row")
            .email_verified
    );

    // Single-use: the bearer is gone.
    assert!(
        store
            .redeem_email_token("tok-confirm-1")
            .expect("redeem")
            .is_none()
    );
    let err = confirm_email(&store, "tok-confirm-1", T0).expect_err("reused");
    assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
}

#[test]
fn confirm_email_wrong_kind_rejected_with_reason() {
    let (_dir, store) = open_store();
    let account = mk_user(&store, "carol", Some("carol@example.com"));
    store_token(
        &store,
        "tok-reset-1",
        account.id,
        EmailTokenKind::PasswordReset,
    );
    let err = confirm_email(&store, "tok-reset-1", T0).expect_err("wrong kind");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    // The address is untouched, and the bearer is consumed (single-use even
    // on the wrong endpoint: no second probe with the same token).
    assert!(
        !store
            .user_by_account_id(account.id)
            .expect("user")
            .expect("row")
            .email_verified
    );
    assert!(
        store
            .redeem_email_token("tok-reset-1")
            .expect("redeem")
            .is_none()
    );
}

#[test]
fn confirm_email_unknown_token() {
    let (_dir, store) = open_store();
    mk_user(&store, "dave", Some("dave@example.com"));
    let err = confirm_email(&store, "no-such-token", T0).expect_err("unknown");
    assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Ownership (grandfather rule)
// ---------------------------------------------------------------------------

#[test]
fn confirmation_ownership_grandfather_and_mismatch() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();

    // CLI-created row: no attribution -> any app is allowed (grandfather).
    let cli = mk_user(&store, "erin", Some("erin@example.com"));
    let cli_user = store
        .user_by_account_id(cli.id)
        .expect("user")
        .expect("row");
    assert_eq!(cli_user.created_by_application, None);
    assert!(check_confirmation_ownership(&cli_user, Id::new(1)).is_ok());
    assert!(check_confirmation_ownership(&cli_user, Id::new(999)).is_ok());

    // Public registration records the signup app.
    let app_a = test_app(9001);
    let app_b = test_app(9002);
    let registered = register_account(
        &store,
        &oauth,
        BASE_URL,
        T0,
        true,
        false,
        &app_a,
        RegisterAccountCommand {
            username: "frank".to_string(),
            email: "frank@example.com".to_string(),
            password: "password123".to_string(),
            agreement: true,
            locale: None,
            reason: None,
            invite_code: None,
            display_name: None,
            time_zone: None,
            date_of_birth: None,
        },
    )
    .expect("register");
    let user = store
        .user_by_account_id(registered.account.id)
        .expect("user")
        .expect("row");
    assert_eq!(user.created_by_application, Some(app_a.id));
    assert!(check_confirmation_ownership(&user, app_a.id).is_ok());
    let err = check_confirmation_ownership(&user, app_b.id).expect_err("mismatch");
    assert!(matches!(err, Error::Forbidden(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// HTTP: resends store-before-enqueue, ownership enforced, public reset
// ---------------------------------------------------------------------------

/// Rebuild the full router sharing the server's store, with mail enabled.
fn enabled_router(srv: &TestServer, from: &str) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let streams = srv.streams.clone();
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(streams.clone()),
    ));
    let server: ServerInfo = (*server_info()).clone();
    let federation = federation_config(context.clone(), TEST_PUBLIC_URL);
    birchtree::api::mastodon::build_router(ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server: Arc::new(server),
        streams: streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: Arc::new(birchtree::mailer::MailEnv::new(from, true)),
        invites: std::sync::Arc::new(
            birchtree::application::accounts::MemoryInviteStore::default(),
        ),
    })
}

fn user_token_for(srv: &TestServer, app_id: Id, account_id: Id) -> String {
    let (plaintext, _) = auth::issue_user_token(
        &*srv.store,
        app_id,
        account_id,
        ["read".to_string(), "write".to_string()].to_vec(),
        T0,
    )
    .expect("token");
    plaintext
}

fn reset_jobs(srv: &TestServer) -> Vec<birchtree::domain::Job> {
    srv.store
        .claim_due(4_000_000_000_000, 64)
        .expect("claim")
        .into_iter()
        .filter(|j| j.kind == JobKind::SendPasswordResetEmail)
        .collect()
}

fn confirmation_jobs(srv: &TestServer) -> Vec<birchtree::domain::Job> {
    srv.store
        .claim_due(4_000_000_000_000, 64)
        .expect("claim")
        .into_iter()
        .filter(|j| j.kind == JobKind::SendConfirmationEmail)
        .collect()
}

#[tokio::test]
async fn resend_stores_token_before_enqueue_and_confirms() {
    let srv = TestServer::new(true);
    let router = enabled_router(&srv, "noreply@example.social");
    // Grandfathered (CLI-created) account: any app token may resend.
    let account = create_account(
        &*srv.store,
        BASE_URL,
        T0,
        CreateAccountCommand {
            username: "gail".to_string(),
            email: Some("gail@example.com".to_string()),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create");
    let app = auth::register_app(
        &*srv.store,
        "gail app".to_string(),
        vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        vec!["read".to_string(), "write".to_string()],
        None,
        false,
        T0,
    )
    .expect("register app");
    let token = user_token_for(&srv, app.app.id, account.id);

    let (status, _, bytes) = send(
        &router,
        form_request("POST", "/api/v1/emails/confirmations", "", Some(&token)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    // The queued job carries the token AND the token row is durable: redeem
    // it straight from the store (proves store-before-enqueue ordering —
    // a crash after enqueue still leaves a redeemable bearer).
    let jobs = confirmation_jobs(&srv);
    assert_eq!(jobs.len(), 1, "one confirmation job queued");
    let mailed = jobs[0].payload["token"].as_str().expect("token in payload");
    assert_eq!(mailed.len(), 64);
    let record = srv
        .store
        .redeem_email_token(mailed)
        .expect("redeem")
        .expect("token row stored");
    assert_eq!(record.account_id, account.id);
    assert_eq!(record.kind, EmailTokenKind::Confirmation);
    for job in &jobs {
        srv.store.complete(job.id, job.due_at_ms).expect("complete");
    }

    // Redeem through the application layer, then the check endpoint reads
    // back verified=true.
    let jobs = confirmation_jobs(&srv);
    assert!(jobs.is_empty(), "already claimed above");
    let (status, _, bytes) = send(
        &router,
        get_request("/api/v1/emails/check_confirmation", Some(&token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(String::from_utf8_lossy(&bytes).trim(), "false");
}

#[tokio::test]
async fn resend_enforces_signup_app_ownership() {
    // Explicitly mail-disabled router: ownership runs before the mail
    // gate, so a mismatch 403s while a match passes through to the 503.
    // (The default test router enables mail so registration tests exercise
    // the fail-closed guard's pass path.)
    let srv = TestServer::new(true);
    let router = srv.disabled_mail_router();
    let app_a = auth::register_app(
        &*srv.store,
        "owner app".to_string(),
        vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        vec!["read".to_string(), "write".to_string()],
        None,
        false,
        T0,
    )
    .expect("register app");
    let app_b = auth::register_app(
        &*srv.store,
        "other app".to_string(),
        vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        vec!["read".to_string(), "write".to_string()],
        None,
        false,
        T0,
    )
    .expect("register app");
    let oauth = &*srv.store;
    let registered = register_account(
        oauth,
        oauth,
        BASE_URL,
        T0,
        true,
        false,
        &app_a.app,
        RegisterAccountCommand {
            username: "heidi".to_string(),
            email: "heidi@example.com".to_string(),
            password: "password123".to_string(),
            agreement: true,
            locale: None,
            reason: None,
            invite_code: None,
            display_name: None,
            time_zone: None,
            date_of_birth: None,
        },
    )
    .expect("register");
    let owner_token = user_token_for(&srv, app_a.app.id, registered.account.id);
    let other_token = user_token_for(&srv, app_b.app.id, registered.account.id);

    // Mismatch: 403 on both the resend and the check endpoints.
    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/emails/confirmations",
            "",
            Some(&other_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let (status, _, bytes) = send(
        &router,
        get_request(
            "/api/v1/emails/check_confirmation",
            Some(&other_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    // Owner: passes ownership (mail-disabled 503 proves the gate passed;
    // check reads back false = unconfirmed).
    let (status, _, _) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/emails/confirmations",
            "",
            Some(&owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _, bytes) = send(
        &router,
        get_request(
            "/api/v1/emails/check_confirmation",
            Some(&owner_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(String::from_utf8_lossy(&bytes).trim(), "false");
}

#[tokio::test]
async fn public_reset_known_queues_unknown_does_not() {
    let srv = TestServer::new(true);
    let router = enabled_router(&srv, "noreply@example.social");
    create_account(
        &*srv.store,
        BASE_URL,
        T0,
        CreateAccountCommand {
            username: "ivan".to_string(),
            email: Some("ivan@example.com".to_string()),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create");

    // Known address, no auth header (public flow): 200 + one reset job.
    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/emails/password_reset",
            "email=ivan@example.com",
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let jobs = reset_jobs(&srv);
    assert_eq!(jobs.len(), 1, "one reset job for the known address");
    let mailed = jobs[0].payload["token"].as_str().expect("token in payload");
    assert_eq!(mailed.len(), 64);
    let record = srv
        .store
        .redeem_email_token(mailed)
        .expect("redeem")
        .expect("token row stored");
    assert_eq!(record.kind, EmailTokenKind::PasswordReset);
    for job in &jobs {
        srv.store.complete(job.id, job.due_at_ms).expect("complete");
    }

    // Unknown address: the same 200, nothing queued (no oracle).
    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/emails/password_reset",
            "email=nobody@example.com",
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(reset_jobs(&srv).is_empty(), "nothing queued for unknown");

    // Missing/empty param: still 200, still nothing.
    let (status, _, _) = send(
        &router,
        form_request("POST", "/api/v1/emails/password_reset", "", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        reset_jobs(&srv).is_empty(),
        "nothing queued without an address"
    );
}

#[tokio::test]
async fn public_reset_case_insensitive_lookup() {
    let srv = TestServer::new(true);
    let router = enabled_router(&srv, "noreply@example.social");
    create_account(
        &*srv.store,
        BASE_URL,
        T0,
        CreateAccountCommand {
            username: "judy".to_string(),
            email: Some("Judy@Example.COM".to_string()),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create");
    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/emails/password_reset",
            "email=judy@example.com",
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(reset_jobs(&srv).len(), 1, "normalized lookup hits");
}
