//! Production fail-closed for signup mail (prod-2 item 4).
//!
//! Every signup path enqueues a `SendConfirmationEmail` job — open
//! registration and invite registration alike. Without an SMTP provider the
//! job can never deliver, so production refuses to serve (startup guard)
//! and the registration endpoint refuses before creating the user (request
//! guard), on both paths. Lab still starts; the mail job itself stays
//! explicitly undelivered (retry, never `Ok` — pinned by
//! `tests/fix_email_trace.rs`).

mod common;

use axum::http::StatusCode;

use birchtree::application::accounts::InviteStore;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::ports::{AccountStore, NewApp, NewToken, OAuthStore};
use birchtree::config::{Config, RuntimeMode};
use birchtree::domain::user::Invite;
use birchtree::domain::{Clock, Id, SystemClock};
use birchtree::mailer::require_mail_for_open_registration;

use common::{TestServer, form_request, send};

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

fn register_app(srv: &TestServer) -> Id {
    srv.store
        .register_app(NewApp {
            name: "mail-guard".to_string(),
            client_id: "mail-guard-client".to_string(),
            client_secret_hash: None,
            redirect_uris: Vec::new(),
            scopes: vec!["read".to_string(), "write".to_string()],
            website: None,
            now_ms: now_ms(),
        })
        .expect("register app")
        .id
}

fn app_token(srv: &TestServer, app_id: Id) -> String {
    let plaintext = "mail-guard-app-token";
    let token_hash = sha256_hex(plaintext.as_bytes());
    srv.store
        .issue_access_token(NewToken {
            app_id,
            user_id: None,
            token_hash,
            scopes: ["write".to_string()].into(),
            now_ms: now_ms(),
            expires_at_ms: None,
        })
        .expect("issue token");
    plaintext.to_string()
}

fn reg_body(username: &str, invite_code: Option<&str>) -> String {
    let mut body = format!(
        "username={username}&email={username}@example.com&password=password123&agreement=true"
    );
    if let Some(code) = invite_code {
        body.push_str(&format!("&invite_code={code}"));
    }
    body
}

fn issue_invite(srv: &TestServer, code: &str) {
    let inviter = common::mk_account(&srv.store, "inviter").id;
    srv.store
        .create_invite(Invite {
            id: Id::new(7001),
            code: code.to_string(),
            created_by_account_id: inviter,
            created_by_staff: false,
            comment: None,
            max_uses: None,
            uses: 0,
            expires_at_ms: None,
            created_at_ms: now_ms(),
        })
        .expect("create invite");
}

/// Production with open registrations and no SMTP fails startup.
#[test]
fn production_open_registration_without_smtp_fails_startup() {
    let cfg = Config {
        registration_open: true,
        ..Config::default()
    };
    assert_eq!(cfg.mode, RuntimeMode::Production);
    assert!(require_mail_for_open_registration(&cfg).is_err());
}

/// Production with closed registrations but enabled invites and no SMTP
/// fails startup too: invite signup enqueues the same confirmation job.
#[test]
fn production_invite_signup_without_smtp_fails_startup() {
    let cfg = Config {
        registration_open: false,
        invites_enabled: true,
        ..Config::default()
    };
    assert!(require_mail_for_open_registration(&cfg).is_err());
}

/// Lab still starts with signup possible and no SMTP.
#[test]
fn lab_signup_without_smtp_still_starts() {
    let cfg = Config {
        mode: RuntimeMode::Lab,
        registration_open: true,
        ..Config::default()
    };
    assert!(require_mail_for_open_registration(&cfg).is_ok());
}

/// Open registration with no SMTP: the endpoint refuses before creating
/// the user.
#[tokio::test]
async fn open_registration_without_mail_creates_no_user() {
    let srv = TestServer::new(true);
    let router = srv.disabled_mail_router();
    let app_id = register_app(&srv);
    let token = app_token(&srv, app_id);

    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/accounts",
            &reg_body("mailguard1", None),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        srv.store
            .account_by_handle("mailguard1", "example.social")
            .expect("lookup")
            .is_none(),
        "no user is created behind the refusal"
    );
}

/// Invite registration with registrations closed and no SMTP: the usable
/// invite opens the closed gate, but the mail guard still refuses before
/// creating the user.
#[tokio::test]
async fn invite_registration_without_mail_creates_no_user() {
    let srv = TestServer::new(false);
    let router = srv.disabled_mail_router();
    let app_id = register_app(&srv);
    let token = app_token(&srv, app_id);
    issue_invite(&srv, "MailGuard1");

    let (status, _, bytes) = send(
        &router,
        form_request(
            "POST",
            "/api/v1/accounts",
            &reg_body("mailguard2", Some("MailGuard1")),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        srv.store
            .account_by_handle("mailguard2", "example.social")
            .expect("lookup")
            .is_none(),
        "no user is created behind the refusal"
    );
}

/// Control: the same invite registration with mail available succeeds, so
/// the refusals above pin the mail guard rather than the invite plumbing.
#[tokio::test]
async fn invite_registration_with_mail_still_succeeds() {
    let srv = TestServer::new(false);
    let app_id = register_app(&srv);
    let token = app_token(&srv, app_id);
    issue_invite(&srv, "MailGuard2");

    let (status, _, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/accounts",
            &reg_body("mailguard3", Some("MailGuard2")),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        srv.store
            .account_by_handle("mailguard3", "example.social")
            .expect("lookup")
            .is_some(),
        "invite signup works when mail is available"
    );
}
