//! Round 3, area A: invite tokens + confirmation/reset mail.
//!
//! Executed evidence for the area report. Covers: invite redeem happy path,
//! invalid/expired/exhausted refusal with reason, confirmation + reset mail
//! enqueued via the log backend, no-config = explicit disabled state, config
//! validation, and the registration×invite×approval policy (D36).

use std::collections::HashMap;
use std::sync::Mutex;

use birchtree::application::accounts::{
    CreateInviteCommand, InviteStore, MemoryInviteStore, RegisterAccountCommand,
    allowed_registration, create_invite, invite_status, redeem_invite,
    register_account_with_invite, request_confirmation_email, request_password_reset,
};
use birchtree::application::ports::{
    AccountStore, NewApp, NewAuthCode, NewToken, OAuthStore,
};
use birchtree::config::Config;
use birchtree::domain::user::{INVITE_COMMENT_LIMIT, Invite, is_valid_invite_code_shape};
use birchtree::domain::{
    AccessToken, AuthorizationCode, Error, Id, OAuthApplication, SystemClock, TimestampIdGenerator,
};
use birchtree::mailer::{DisabledMailer, LogMailer, MailError, Mailer, mailer_from_config};
use birchtree::persistence::redb::RedbStore;

const T0: u64 = 1_700_000_000_000;
const BASE_URL: &str = "https://example.social";

fn open_store() -> (tempfile::TempDir, RedbStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = RedbStore::open(
        dir.path().join("birchtree.redb"),
        std::sync::Arc::new(SystemClock),
        std::sync::Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
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
    fn register_app(&self, _input: NewApp) -> Result<OAuthApplication, Error> {
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
    fn create_authorization_code(
        &self,
        _input: NewAuthCode,
    ) -> Result<AuthorizationCode, Error> {
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

fn test_app() -> OAuthApplication {
    OAuthApplication {
        id: Id::new(9001),
        name: "test app".to_string(),
        client_id: "client-id".to_string(),
        client_secret_hash: None,
        redirect_uris: vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        scopes: vec!["read".to_string(), "write".to_string()],
        website: None,
        created_at_ms: T0,
    }
}

fn register_cmd(username: &str, invite_code: Option<&str>) -> RegisterAccountCommand {
    RegisterAccountCommand {
        username: username.to_string(),
        email: "user@example.com".to_string(),
        password: "password123".to_string(),
        agreement: true,
        locale: None,
        reason: None,
        invite_code: invite_code.map(str::to_string),
        display_name: None,
    }
}

fn issue(invites: &MemoryInviteStore, staff: bool, max_uses: Option<u32>) -> Invite {
    create_invite(
        invites,
        T0,
        CreateInviteCommand {
            created_by_account_id: Id::new(1),
            created_by_staff: staff,
            comment: None,
            max_uses,
            expires_in_ms: None,
        },
    )
    .expect("issue invite")
}

#[test]
fn invite_codes_have_upstream_shape() {
    let invites = MemoryInviteStore::new();
    let a = issue(&invites, false, None);
    let b = issue(&invites, false, None);
    assert!(is_valid_invite_code_shape(&a.code), "code {}", a.code);
    assert!(is_valid_invite_code_shape(&b.code), "code {}", b.code);
    assert_ne!(a.code, b.code);
    assert_eq!(a.code.len(), 8);
}

#[test]
fn invite_redeem_happy_path() {
    let invites = MemoryInviteStore::new();
    let invite = issue(&invites, false, Some(2));
    assert_eq!(invite.uses, 0);

    let status = invite_status(&invites, &invite.code, T0).expect("usable");
    assert_eq!(status.code, invite.code);

    let first = redeem_invite(&invites, &invite.code, T0).expect("first redeem");
    assert_eq!(first.uses, 1);
    let second = redeem_invite(&invites, &invite.code, T0).expect("second redeem");
    assert_eq!(second.uses, 2);
}

#[test]
fn unknown_code_is_not_found() {
    let invites = MemoryInviteStore::new();
    let err = invite_status(&invites, "ZZZZZZZZ", T0).expect_err("unknown code");
    assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
    let err = redeem_invite(&invites, "ZZZZZZZZ", T0).expect_err("unknown code");
    assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
}

#[test]
fn expired_token_refused_with_reason() {
    let invites = MemoryInviteStore::new();
    let invite = create_invite(
        &invites,
        T0,
        CreateInviteCommand {
            created_by_account_id: Id::new(1),
            created_by_staff: false,
            comment: None,
            max_uses: None,
            expires_in_ms: Some(1_000),
        },
    )
    .expect("issue");
    // Still usable before expiry.
    assert!(invite_status(&invites, &invite.code, T0 + 999).is_ok());
    // Refused at and past expiry, with the upstream-style reason.
    for now in [T0 + 1_000, T0 + 60_000] {
        let err = invite_status(&invites, &invite.code, now).expect_err("expired");
        assert!(
            matches!(err, Error::Unauthorized(ref m) if m == "Invalid invitation code"),
            "got {err:?}"
        );
        let err = redeem_invite(&invites, &invite.code, now).expect_err("expired");
        assert!(
            matches!(err, Error::Unauthorized(ref m) if m == "Invalid invitation code"),
            "got {err:?}"
        );
    }
}

#[test]
fn exhausted_token_refused_with_reason() {
    let invites = MemoryInviteStore::new();
    let invite = issue(&invites, false, Some(1));
    redeem_invite(&invites, &invite.code, T0).expect("single use");
    let err = invite_status(&invites, &invite.code, T0).expect_err("exhausted");
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m == "Invalid invitation code"),
        "got {err:?}"
    );
    let err = redeem_invite(&invites, &invite.code, T0).expect_err("exhausted");
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m == "Invalid invitation code"),
        "got {err:?}"
    );
}

#[test]
fn invite_inputs_validated() {
    let invites = MemoryInviteStore::new();
    let err = create_invite(
        &invites,
        T0,
        CreateInviteCommand {
            created_by_account_id: Id::new(1),
            created_by_staff: false,
            comment: Some("x".repeat(INVITE_COMMENT_LIMIT + 1)),
            max_uses: None,
            expires_in_ms: None,
        },
    )
    .expect_err("long comment");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    let err = create_invite(
        &invites,
        T0,
        CreateInviteCommand {
            created_by_account_id: Id::new(1),
            created_by_staff: false,
            comment: None,
            max_uses: Some(0),
            expires_in_ms: None,
        },
    )
    .expect_err("zero max_uses");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
}

#[test]
fn registration_policy_matches_upstream_helper() {
    // RegistrationHelper#allowed_registration?: open OR usable invite.
    assert!(allowed_registration(true, false));
    assert!(allowed_registration(true, true));
    assert!(allowed_registration(false, true));
    assert!(!allowed_registration(false, false));
}

#[test]
fn closed_registrations_with_valid_invite_registers() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();
    let invites = MemoryInviteStore::new();
    let invite = issue(&invites, false, Some(5));

    let registered = register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        false, // closed
        false,
        true, // invites enabled
        &test_app(),
        register_cmd("invited", Some(&invite.code)),
    )
    .expect("invite opens closed registration");
    assert!(!registered.token_plaintext.is_empty());
    let after = invites
        .invite_by_code(&invite.code)
        .expect("lookup")
        .expect("present");
    assert_eq!(after.uses, 1);
}

#[test]
fn closed_registrations_without_invite_refused() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();
    let invites = MemoryInviteStore::new();
    let err = register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        false,
        false,
        true,
        &test_app(),
        register_cmd("nobody", None),
    )
    .expect_err("closed");
    assert!(matches!(err, Error::Forbidden(_)), "got {err:?}");
}

#[test]
fn unknown_code_while_open_is_ignored_like_upstream() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();
    let invites = MemoryInviteStore::new();
    register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        true, // open: unknown code ignored (upstream parity)
        false,
        true,
        &test_app(),
        register_cmd("opencode", Some("ZZZZZZZZ")),
    )
    .expect("proceeds while open");
}

#[test]
fn invites_disabled_refuses_code_with_reason() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();
    let invites = MemoryInviteStore::new();
    let invite = issue(&invites, false, None);
    let err = register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        true,
        false,
        false, // invites disabled
        &test_app(),
        register_cmd("coded", Some(&invite.code)),
    )
    .expect_err("disabled");
    assert!(
        matches!(err, Error::InvalidInput(ref m) if m == "invites are disabled"),
        "got {err:?}"
    );
}

#[test]
fn staff_invite_bypasses_approval_others_queue() {
    let (_dir, store) = open_store();
    let oauth = FakeOAuth::default();
    let invites = MemoryInviteStore::new();
    let staff_invite = issue(&invites, true, None);
    let user_invite = issue(&invites, false, None);

    let staff_reg = register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        true,
        true, // approval required (D36 queue)
        true,
        &test_app(),
        register_cmd("staffinvited", Some(&staff_invite.code)),
    )
    .expect("staff invite registers");
    let staff_user = store
        .user_by_account_id(staff_reg.account.id)
        .expect("lookup")
        .expect("present");
    assert!(staff_user.approved, "staff invite bypasses approval");

    let user_reg = register_account_with_invite(
        &store,
        &oauth,
        &invites,
        BASE_URL,
        T0,
        true,
        true,
        true,
        &test_app(),
        register_cmd("userinvited", Some(&user_invite.code)),
    )
    .expect("user invite registers");
    let user = store
        .user_by_account_id(user_reg.account.id)
        .expect("lookup")
        .expect("present");
    assert!(!user.approved, "non-staff invite still queues for approval");
}

#[test]
fn confirmation_mail_enqueued_via_log_backend() {
    let mailer = LogMailer::new();
    request_confirmation_email(
        &mailer,
        "no-reply@example.social",
        "newuser@example.com",
        Id::new(42),
        "confirm-token-abc",
        BASE_URL,
    )
    .expect("queue");
    let out = mailer.outbox();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].from, "no-reply@example.social");
    assert_eq!(out[0].to, "newuser@example.com");
    assert_eq!(out[0].subject, "Confirmation instructions");
    assert!(out[0].body_text.contains("confirm-token-abc"));
    assert!(out[0].body_text.contains("/auth/confirmation?confirmation_token="));
}

#[test]
fn password_reset_mail_enqueued_via_log_backend() {
    let mailer = LogMailer::new();
    request_password_reset(
        &mailer,
        "no-reply@example.social",
        "existing@example.com",
        Id::new(43),
        "reset-token-xyz",
        BASE_URL,
    )
    .expect("queue");
    let out = mailer.outbox();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].subject, "Reset password instructions");
    assert!(out[0].body_text.contains("reset-token-xyz"));
    assert!(out[0].body_text.contains("/auth/password/edit?reset_password_token="));
}

#[test]
fn no_mail_config_is_explicit_disabled_state() {
    // Production default: no provider -> every send fails loudly.
    let mailer = mailer_from_config(None, None).expect("builds");
    let err = request_confirmation_email(
        &*mailer,
        "",
        "u@example.com",
        Id::new(1),
        "tok",
        BASE_URL,
    )
    .expect_err("disabled");
    assert!(matches!(err, MailError::Disabled(_)), "got {err:?}");
    let err = request_password_reset(&*mailer, "", "u@example.com", Id::new(1), "tok", BASE_URL)
        .expect_err("disabled");
    assert!(matches!(err, MailError::Disabled(_)), "got {err:?}");
    // The domain conversion is a refusal, not a success.
    let domain: Error = MailError::Disabled("x").into();
    assert!(matches!(domain, Error::Forbidden(_)), "got {domain:?}");

    // The bare disabled backend behaves the same.
    let bare = DisabledMailer;
    assert!(matches!(
        Mailer::send_confirmation(&bare, "", "u@example.com", Id::new(1), "tok", BASE_URL),
        Err(MailError::Disabled(_))
    ));
}

#[test]
fn mail_config_misconfiguration_is_a_hard_error() {
    let mut cfg = Config::default();
    assert!(!cfg.mail_enabled());
    assert!(cfg.validate().is_ok());

    cfg.mail_from = Some("no-reply@example.social".to_string());
    assert!(cfg.validate().is_err(), "from without provider must fail");

    cfg = Config::default();
    cfg.smtp_url = Some("http://mail:25".to_string());
    cfg.mail_from = Some("no-reply@example.social".to_string());
    assert!(cfg.validate().is_err(), "non-SMTP scheme must fail");

    cfg = Config::default();
    cfg.smtp_url = Some("smtp://mail:25".to_string());
    assert!(cfg.validate().is_err(), "missing sender must fail");
    assert!(mailer_from_config(cfg.smtp_url.as_deref(), cfg.mail_from.as_deref()).is_err());

    cfg.mail_from = Some("no-reply@example.social".to_string());
    assert!(cfg.validate().is_ok());
    assert!(cfg.mail_enabled());
    assert!(mailer_from_config(cfg.smtp_url.as_deref(), cfg.mail_from.as_deref()).is_ok());
}
