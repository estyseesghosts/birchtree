//! Phase 7.3 roles + scope plumbing (Subagent A).
//!
//! Direct tests of the role-before-scope gate
//! ([`Authenticated::require_admin`]), the staff-role store plumbing
//! ([`AccountStore::set_user_role`]), and the admin scope registry. No admin
//! routes are exercised here — routes belong to the sibling workstreams; the
//! gate is tested through its helper signature exactly as handlers call it.
//!
//! Upstream findings encoded here (verified 2026-10-20 against the pinned
//! v4.7.3 sources):
//! - Role check runs before the scope check: `Admin::PermissionsConcern`'s
//!   `before_action` (role) precedes each controller's `authorize_if_got_token!`
//!   (scope) `before_action`, with `verify_authorized` (policy) last.
//! - A role failure renders `403 {"error":"Forbidden"}`: `ErrorResponses#forbidden`
//!   calls `respond_with_error(403)`, which renders
//!   `{ error: Rack::Utils::HTTP_STATUS_CODES[403] }` = `{"error":"Forbidden"}`.
//! - The permission split is a decision, not upstream's shape: upstream's
//!   `Admin::BasePolicy#access?` admits any moderation/administration/devops
//!   flag and each controller's `authorize` call narrows per action. Birchtree
//!   collapses this to two roles per `birchtree.md` §7: `moderator` serves the
//!   accounts + reports areas, `admin` serves every admin area.

mod common;

use axum::http::StatusCode;
use birchtree::api::mastodon::auth::{AdminArea, Authenticated};
use birchtree::api::oauth::scopes;
use birchtree::application::auth as auth_uc;
use birchtree::application::ports::AccountStore;
use birchtree::domain::{AccessToken, LocalUser, OAuthApplication, UserRole};

const ACCOUNTS_READ: &[&str] = &["admin:read", "admin:read:accounts"];
const ACCOUNTS_WRITE: &[&str] = &["admin:write", "admin:write:accounts"];
const REPORTS_WRITE: &[&str] = &["admin:write", "admin:write:reports"];
const DOMAIN_BLOCKS_READ: &[&str] = &["admin:read", "admin:read:domain_blocks"];

struct Fixture {
    _dir: tempfile::TempDir,
    store: std::sync::Arc<birchtree::persistence::redb::RedbStore>,
    app: OAuthApplication,
}

impl Fixture {
    fn new() -> Self {
        let (dir, store) = common::temp_store();
        let registered = auth_uc::register_app(
            &*store,
            "phase7-3-roles".to_string(),
            vec!["https://client.example/cb".to_string()],
            vec![
                "admin:read".to_string(),
                "admin:write".to_string(),
                "admin:read:accounts".to_string(),
                "admin:write:accounts".to_string(),
                "admin:read:reports".to_string(),
                "admin:write:reports".to_string(),
                "admin:read:domain_blocks".to_string(),
                "read".to_string(),
            ],
            None,
            false,
            common::T0,
        )
        .expect("register app");
        Self {
            _dir: dir,
            store,
            app: registered.app,
        }
    }

    /// A user token for `username` (created if needed) carrying `scopes`.
    /// `role` is granted first when staff.
    fn user_token(&self, username: &str, role: UserRole, scopes: &[&str]) -> Authenticated {
        let account = match self
            .store
            .account_by_handle(username, "example.social")
            .expect("store")
        {
            Some(account) => account,
            None => common::mk_account(&self.store, username),
        };
        if role.is_staff() {
            self.store
                .set_user_role(account.id, role)
                .expect("grant role");
        }
        let (_, token) = auth_uc::issue_user_token(
            &*self.store,
            self.app.id,
            account.id,
            scopes.iter().map(|s| s.to_string()).collect(),
            common::T0,
        )
        .expect("issue user token");
        Authenticated {
            token,
            app: self.app.clone(),
            approved: true,
        }
    }

    /// An app-only (client-credentials) token carrying `scopes`.
    fn app_token(&self, scopes: &[&str]) -> Authenticated {
        let (_, token) = auth_uc::issue_client_credentials_token(
            &*self.store,
            self.app.id,
            scopes.iter().map(|s| s.to_string()).collect(),
            common::T0,
        )
        .expect("issue app token");
        Authenticated {
            token,
            app: self.app.clone(),
            approved: true,
        }
    }
}

fn forbidden_body(err: &birchtree::api::mastodon::errors::ApiError) -> (StatusCode, String) {
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.message, "Forbidden");
    (err.status, err.message.clone())
}

// -- Role gate: ordering and exact errors -----------------------------------

#[test]
fn role_less_user_with_admin_scope_gets_forbidden() {
    let fx = Fixture::new();
    let auth = fx.user_token("plain", UserRole::None, ACCOUNTS_READ);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_READ)
        .expect_err("role-less user must be refused");
    forbidden_body(&err);
}

#[tokio::test]
async fn forbidden_renders_exact_error_body() {
    use axum::response::IntoResponse;
    let fx = Fixture::new();
    let auth = fx.user_token("plain2", UserRole::None, ACCOUNTS_READ);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Reports, REPORTS_WRITE)
        .expect_err("role-less user must be refused");
    let response = err.into_response();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("body");
    assert_eq!(&body[..], br#"{"error":"Forbidden"}"#);
}

#[test]
fn scoped_out_admin_gets_scopes_error() {
    let fx = Fixture::new();
    // Admin role, but the token lacks the write scope the route needs.
    let auth = fx.user_token("boss", UserRole::Admin, ACCOUNTS_READ);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_WRITE)
        .expect_err("scoped-out admin must be refused");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.message, "This action is outside the authorized scopes");
}

#[test]
fn app_only_token_gets_422_before_any_role_check() {
    let fx = Fixture::new();
    // App-only token carrying full admin scopes: the user check (422) runs
    // before the role check, so this is never a 403.
    let auth = fx.app_token(ACCOUNTS_WRITE);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_WRITE)
        .expect_err("app-only token must be refused");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err.message, "This method requires an authenticated user");
}

#[test]
fn role_check_runs_before_scope_check() {
    let fx = Fixture::new();
    // Role-less AND scoped-out: the role refusal wins (upstream order:
    // `PermissionsConcern` before the controller's scope `before_action`).
    let auth = fx.user_token("plain3", UserRole::None, &["read"]);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_WRITE)
        .expect_err("must be refused");
    forbidden_body(&err);
}

#[test]
fn moderator_without_scope_gets_scopes_error() {
    let fx = Fixture::new();
    // Moderator passes the role check for reports, then fails the scope check.
    let auth = fx.user_token("mod", UserRole::Moderator, ACCOUNTS_READ);
    let err = auth
        .require_admin(&*fx.store, AdminArea::Reports, REPORTS_WRITE)
        .expect_err("scoped-out moderator must be refused");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.message, "This action is outside the authorized scopes");
}

// -- Permission split: moderator = accounts+reports; admin = everything ----

#[test]
fn moderator_reaches_accounts_and_reports_but_not_admin_only() {
    let fx = Fixture::new();
    let auth = fx.user_token("mod2", UserRole::Moderator, &["admin:read", "admin:write"]);
    assert_eq!(
        auth.require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_READ)
            .expect("moderator reaches accounts"),
        UserRole::Moderator
    );
    assert_eq!(
        auth.require_admin(&*fx.store, AdminArea::Reports, REPORTS_WRITE)
            .expect("moderator reaches reports"),
        UserRole::Moderator
    );
    let err = auth
        .require_admin(&*fx.store, AdminArea::AdminOnly, DOMAIN_BLOCKS_READ)
        .expect_err("moderator must not reach domain blocks");
    forbidden_body(&err);
}

#[test]
fn admin_reaches_every_area() {
    let fx = Fixture::new();
    let auth = fx.user_token(
        "root",
        UserRole::Admin,
        &["admin:read", "admin:write", "admin:read:domain_blocks"],
    );
    for area in [AdminArea::Accounts, AdminArea::Reports, AdminArea::AdminOnly] {
        let scopes: &[&str] = match area {
            AdminArea::Accounts => ACCOUNTS_READ,
            AdminArea::Reports => REPORTS_WRITE,
            // The moderator test token above lacks the granular sub-scope, so
            // the bare `admin:read` alternative must also grant access.
            AdminArea::AdminOnly => &["admin:read"],
        };
        assert_eq!(
            auth.require_admin(&*fx.store, area, scopes)
                .expect("admin reaches every area"),
            UserRole::Admin
        );
    }
}

#[test]
fn broad_scope_alone_grants_access() {
    // Upstream scope checks are "any-of" (broad OR granular): a token with
    // only `admin:write` passes a route accepting
    // `["admin:write", "admin:write:accounts"]`.
    let fx = Fixture::new();
    let auth = fx.user_token("boss2", UserRole::Admin, &["admin:write"]);
    assert_eq!(
        auth.require_admin(&*fx.store, AdminArea::Accounts, ACCOUNTS_WRITE)
            .expect("broad scope suffices"),
        UserRole::Admin
    );
}

// -- Store plumbing ----------------------------------------------------------

#[test]
fn set_user_role_persists_grant_and_clear() {
    let fx = Fixture::new();
    let account = common::mk_account(&fx.store, "staffer");
    assert_eq!(
        fx.store
            .user_by_account_id(account.id)
            .expect("store")
            .expect("user")
            .role,
        UserRole::None,
        "new accounts are role-less"
    );
    let updated = fx
        .store
        .set_user_role(account.id, UserRole::Moderator)
        .expect("grant");
    assert_eq!(updated.role, UserRole::Moderator);
    assert_eq!(
        fx.store
            .user_by_account_id(account.id)
            .expect("store")
            .expect("user")
            .role,
        UserRole::Moderator,
        "grant is visible on re-read (one committed txn)"
    );
    let cleared = fx
        .store
        .set_user_role(account.id, UserRole::None)
        .expect("clear");
    assert_eq!(cleared.role, UserRole::None);
}

#[test]
fn set_user_role_on_unknown_account_is_not_found() {
    let fx = Fixture::new();
    let err = fx
        .store
        .set_user_role(birchtree::domain::Id::new(999_999), UserRole::Admin)
        .expect_err("unknown account must fail");
    assert!(
        matches!(err, birchtree::domain::Error::NotFound(_)),
        "unexpected error: {err:?}"
    );
}

#[test]
fn pre_role_records_decode_as_role_less() {
    // Records written before Phase 7.3 carry no `role` key; `#[serde(default)]`
    // decodes them as `UserRole::None`, so no migration/backfill is needed.
    let legacy = serde_json::json!({
        "id": 7u64,
        "account_id": 8u64,
        "password_hash": "argon2$placeholder",
        "email": null,
        "email_verified": false,
        "created_at_ms": 1_700_000_000_000u64,
        "disabled": false
    });
    let user: LocalUser = serde_json::from_value(legacy).expect("legacy decodes");
    assert_eq!(user.role, UserRole::None);
    assert!(!user.role.is_staff());
}

#[test]
fn role_cli_names_parse() {
    assert_eq!(UserRole::parse_cli("admin"), Some(UserRole::Admin));
    assert_eq!(UserRole::parse_cli("moderator"), Some(UserRole::Moderator));
    assert_eq!(UserRole::parse_cli("none"), Some(UserRole::None));
    assert_eq!(UserRole::parse_cli("ADMIN"), Some(UserRole::Admin));
    assert_eq!(UserRole::parse_cli("root"), None);
    assert_eq!(UserRole::parse_cli(""), None);
}

// -- Scope registry ----------------------------------------------------------

#[test]
fn admin_scope_families_are_known() {
    for scope in [
        "admin:read",
        "admin:write",
        "admin:read:accounts",
        "admin:write:accounts",
        "admin:read:reports",
        "admin:write:reports",
        "admin:read:domain_allows",
        "admin:write:domain_allows",
        "admin:read:domain_blocks",
        "admin:write:domain_blocks",
        "admin:read:ip_blocks",
        "admin:write:ip_blocks",
        "admin:read:email_domain_blocks",
        "admin:write:email_domain_blocks",
        "admin:read:canonical_email_blocks",
        "admin:write:canonical_email_blocks",
    ] {
        assert!(scopes::is_known(scope), "{scope} must be registered");
    }
}

#[test]
fn no_sub_scopes_are_invented_for_trends_tags_or_metrics() {
    // Upstream uses the bare `admin:read` / `admin:write` for trends, tags,
    // measures, dimensions and retention — no sub-scopes exist for those.
    for scope in [
        "admin:read:tags",
        "admin:write:tags",
        "admin:read:trends",
        "admin:read:measures",
        "admin:read:dimensions",
        "admin:read:retention",
    ] {
        assert!(!scopes::is_known(scope), "{scope} must NOT be registered");
    }
    // The bare scopes the trends/tags/metrics routes take are known.
    assert!(scopes::is_known("admin:read"));
    assert!(scopes::is_known("admin:write"));
}

#[test]
fn token_type_is_preserved_for_gate() {
    // Sanity: an app-only token really has no user id, so the gate's 422 comes
    // from `require_user`, not from a missing role row.
    let fx = Fixture::new();
    let auth = fx.app_token(&["admin:read"]);
    assert!(
        auth.token.user_id.is_none(),
        "fixture app token must be app-only"
    );
    let _: AccessToken = auth.token;
}
