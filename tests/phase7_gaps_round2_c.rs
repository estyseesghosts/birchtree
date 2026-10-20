//! Phase 7.3 gap-closing round 2, subagent C: D36 (registration approval
//! modes), D37 (Rule model + account warnings), D39 (sign-in telemetry +
//! feasible metric keys).
//!
//! Self-contained harness: the D36 tests need an approval-mode server
//! (`registration_requires_approval`), so assembly is replicated here
//! instead of sharing `TestServer` (which has no approval switch).
//!
//! Upstream pins (Mastodon v4.7.3, `8ff11c2b`):
//! - D36: `User#approved` + `scope :pending`, `AccountFilter` pending scope,
//!   `User#approve!` (`enable!` when disabled + `approved = true`),
//!   `UserPolicy#approve?/#reject?` (both require `!approved`),
//!   `Api::BaseController#require_user!` → `403 Your login is currently
//!   pending approval`.
//! - D37: `Rule` (`text`, `hint`), `Report#rules` + `validate_rule_ids`
//!   (`reports.errors.invalid_rules`), `Admin::AccountAction` strike
//!   (`AccountWarning`) on every type; the `none`-without-`report_id` 422
//!   stays.
//! - D39: `Admin::Metrics::Measure` / `Dimension` key sets; served keys carry
//!   real stored data, refused keys stay omitted (upstream `filter_map`
//!   drops unknown keys) with per-key reasons in the endpoint manifest.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};

use birchtree::api::mastodon::{ApiState, ServerInfo, build_router};
use birchtree::application::admin as admin_uc;
use birchtree::application::context::Context;
use birchtree::application::ports::{AccountStore, AdminInfraStore, AdminStore, OAuthStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

use common::{TestServer, form_request, get_request, json, send};

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A fresh server whose public registration creates pending accounts when
/// `require_approval` is set (D36 operator switch under test).
struct ApprovalSrv {
    _dir: tempfile::TempDir,
    store: Arc<RedbStore>,
    router: axum::Router,
}

fn approval_server(require_approval: bool) -> ApprovalSrv {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(common::open_store(dir.path()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs: Arc<birchtree::media::blob_store::FsBlobStore> = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.path().join("media"))
            .expect("open blob store"),
    );
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        registration_open: true,
        registration_requires_approval: require_approval,
        transcoding_enabled: false,
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let streams = Hub::new(store.clone(), clock.clone(), server.clone(), Limits::default());
    let context = Arc::new(Context::new(
        clock,
        ids,
        store.clone(),
        blobs,
        Arc::new(streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let router = build_router(ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
    });
    ApprovalSrv {
        _dir: dir,
        store,
        router,
    }
}

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses write:statuses read:follows write:follows write:reports admin:read admin:write admin:read:accounts admin:write:accounts admin:read:reports admin:write:reports";

async fn register_app(router: &axum::Router, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) = send(router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(router: &axum::Router, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) = send(router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "scopes={scopes} body={}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(
    router: &axum::Router,
    app: &serde_json::Value,
    username: &str,
) -> (String, serde_json::Value) {
    let app = app_token(router, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) =
        send(router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    (body["access_token"].as_str().unwrap().to_string(), body)
}

fn post_admin(uri: &str, body: &str, token: Option<&str>) -> Request<Body> {
    form_request("POST", uri, body, token)
}

/// Mint a user token directly (the operator bootstrap path, bypassing the
/// password login that pending accounts cannot complete).
fn mint_user_token(store: &RedbStore, app: &serde_json::Value, account_id: Id) -> String {
    let oauth: &dyn OAuthStore = store;
    let app_id: Id = app["id"].as_str().unwrap().parse().expect("app id");
    let (plaintext, _) = birchtree::application::auth::issue_user_token(
        oauth,
        app_id,
        account_id,
        ALL_SCOPES.split(' ').map(str::to_string).collect(),
        common::T0,
    )
    .expect("issue user token");
    plaintext
}

/// File a report through the public API.
async fn file_report(router: &axum::Router, reporter: &str, target: Id, extra: &str) -> serde_json::Value {
    let body = format!("account_id={target}&comment=spammy{extra}");
    let (status, _h, bytes) =
        send(router, form_request("POST", "/api/v1/reports", &body, Some(reporter))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// ---------------------------------------------------------------------------
// D36: registration approval modes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d36_pending_queue_approve_reject() {
    let srv = approval_server(true);
    let app = register_app(&srv.router, ALL_SCOPES).await;

    // Operator bootstrap: a CLI-created admin (always approved).
    let admin = common::mk_account(&srv.store, "boss");
    srv.store
        .set_user_role(admin.id, UserRole::Admin)
        .expect("grant role");
    let admin_token = mint_user_token(&srv.store, &app, admin.id);

    // Public registration lands pending.
    let (pending_token, _) = register_account(&srv.router, &app, "newbie").await;
    let pending_id = srv
        .store
        .account_by_handle("newbie", "example.social")
        .expect("lookup")
        .expect("account")
        .id;
    assert!(
        !srv
            .store
            .user_by_account_id(pending_id)
            .expect("user")
            .expect("credential")
            .approved
    );

    // Serializer shows the pending state.
    let (status, _, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/admin/accounts/{pending_id}"),
            Some(&admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["approved"], false);

    // The pending filter matches (v1 flag and v2 status spellings).
    for uri in [
        "/api/v1/admin/accounts?pending=1".to_string(),
        "/api/v2/admin/accounts?status=pending".to_string(),
    ] {
        let (status, _, bytes) =
            send(&srv.router, get_request(&uri, Some(&admin_token), None)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let rows = json(&bytes);
        assert!(
            rows.as_array().unwrap().iter().any(|r| r["id"] == pending_id.to_string()),
            "{uri}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    // A pending token is refused by user-token endpoints (upstream
    // `require_user!` → 403 Your login is currently pending approval).
    let (status, _, bytes) = send(
        &srv.router,
        get_request(
            "/api/v1/accounts/verify_credentials",
            Some(&pending_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Your login is currently pending approval");

    // Lookup surfaces omit pending accounts (upstream `without_unapproved`
    // on multi-fetch, `check_account_approval` → 404 on show).
    let (status, _, _) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{pending_id}"), Some(&admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts?id[]={pending_id}"), Some(&admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);

    // Password login refuses pending accounts (no session cookie).
    let login_body = format!(
        "username={}&password=password123&client_id={}&redirect_uri={}&response_type=code&scope=read&state=x",
        enc("newbie"),
        enc(app["client_id"].as_str().unwrap()),
        enc("https://client.example/cb"),
    );
    let (status, headers, _) = send(
        &srv.router,
        form_request("POST", "/oauth/authorize", &login_body, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        common::session_cookie(&headers).is_none(),
        "pending login must not establish a session"
    );

    // Approve enables: 200, approved + re-enabled, audited.
    let (status, _, bytes) = send(
        &srv.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{pending_id}/approve"),
            "",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["approved"], true);
    assert_eq!(json(&bytes)["disabled"], false);
    let entries = srv.store.admin_audit_entries(100).expect("audit");
    assert!(entries.iter().any(|e| e.action == "approve" && e.target_id == pending_id.to_string()));

    // The token works now.
    let (status, _, _) = send(
        &srv.router,
        get_request(
            "/api/v1/accounts/verify_credentials",
            Some(&pending_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // ... and the account is visible on lookup surfaces again.
    let (status, _, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{pending_id}"), Some(&admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["id"], pending_id.to_string());

    // Approving an approved account → 403 (upstream `UserPolicy#approve?`).
    let (status, _, bytes) = send(
        &srv.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{pending_id}/approve"),
            "",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");

    // Reject suspends + disables the next pending account (D38 will replace
    // this with the async deletion worker; upstream deletes here).
    let (pending2_token, _) = register_account(&srv.router, &app, "newbie2").await;
    let pending2_id = srv
        .store
        .account_by_handle("newbie2", "example.social")
        .expect("lookup")
        .expect("account")
        .id;
    let (status, _, bytes) = send(
        &srv.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{pending2_id}/reject"),
            "",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!({}));
    let (status, _, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/admin/accounts/{pending2_id}"),
            Some(&admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["suspended"], true);
    assert_eq!(json(&bytes)["disabled"], true);
    let entries = srv.store.admin_audit_entries(100).expect("audit");
    assert!(entries.iter().any(|e| e.action == "reject" && e.target_id == pending2_id.to_string()));

    // D24 interaction (owned by subagent B; asserted here for the
    // synthesis): the rejected account's token is killed → 401 on reuse.
    let (status, _, _) = send(
        &srv.router,
        get_request(
            "/api/v1/accounts/verify_credentials",
            Some(&pending2_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn d36_approval_off_registers_approved() {
    // Default mode: registration stays open-and-approved; nothing is pending.
    let srv = approval_server(false);
    let app = register_app(&srv.router, ALL_SCOPES).await;
    let admin = common::mk_account(&srv.store, "boss");
    srv.store
        .set_user_role(admin.id, UserRole::Admin)
        .expect("grant role");
    let admin_token = mint_user_token(&srv.store, &app, admin.id);

    let (token, _) = register_account(&srv.router, &app, "regular").await;
    let id = srv
        .store
        .account_by_handle("regular", "example.social")
        .expect("lookup")
        .expect("account")
        .id;
    assert!(
        srv.store
            .user_by_account_id(id)
            .expect("user")
            .expect("credential")
            .approved
    );
    let (status, _, _) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Nothing is pending.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v2/admin/accounts?status=pending", Some(&admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// D37: Rule model + account warnings.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d37_report_rules_populated_and_warnings() {
    let srv = TestServer::new(true);
    let app = register_app(&srv.router, ALL_SCOPES).await;
    let (admin_token, _) = register_account(&srv.router, &app, "boss").await;
    let (mod_token, _) = register_account(&srv.router, &app, "mod").await;
    let (user_token, _) = register_account(&srv.router, &app, "plain").await;
    let account_id = |name: &str| {
        srv.store
            .account_by_handle(name, "example.social")
            .expect("lookup")
            .expect("account")
            .id
    };
    let admin_id = account_id("boss");
    let mod_id = account_id("mod");
    let user_id = account_id("plain");
    srv.store.set_user_role(admin_id, UserRole::Admin).expect("role");
    srv.store.set_user_role(mod_id, UserRole::Moderator).expect("role");

    // A server rule, created through the use case (CLI `create-rule` in prod).
    let infra: &dyn AdminInfraStore = &*srv.store;
    let rule = admin_uc::create_rule(infra, "No spam", "Commercial spam", common::T0)
        .expect("create rule");
    assert_eq!(rule.text, "No spam");

    // The target posts; the reporter files a report naming the status.
    let (status, _, status_bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=hello+world&visibility=public", Some(&mod_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&status_bytes));
    let status_id = json(&status_bytes)["id"].as_str().unwrap().to_string();
    let report = file_report(&srv.router, &user_token, mod_id, &format!("&status_ids[]={status_id}")).await;
    let report_id = report["id"].as_str().unwrap().to_string();

    // Attaching the rule renders it (no longer `[]`).
    let uri = format!("/api/v1/admin/reports/{report_id}");
    let (status, _, bytes) = send(
        &srv.router,
        Request::builder()
            .method("PUT")
            .uri(uri.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", format!("Bearer {mod_token}"))
            .body(Body::from(format!("category=violation&rule_ids[]={}", rule.id)))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rules = json(&bytes)["rules"].clone();
    assert_eq!(rules.as_array().unwrap().len(), 1);
    assert_eq!(rules[0]["id"], rule.id.to_string());
    assert_eq!(rules[0]["text"], "No spam");

    // Unknown rule ids are refused 422 (upstream `validate_rule_ids`).
    let (status, _, _) = send(
        &srv.router,
        Request::builder()
            .method("PUT")
            .uri(uri.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", format!("Bearer {mod_token}"))
            .body(Body::from("category=violation&rule_ids[]=999999999999"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Warn with the report: 200 + a stored warning row carrying the text,
    // the report link and the reported status.
    let action_uri = format!("/api/v1/admin/accounts/{mod_id}/action");
    let (status, _, bytes) = send(
        &srv.router,
        post_admin(
            &action_uri,
            &format!("type=none&report_id={report_id}&text=final+warning"),
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let warnings = srv.store.admin_warnings_for_account(mod_id).expect("warnings");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].account_id, admin_id);
    assert_eq!(warnings[0].action, "none");
    assert_eq!(warnings[0].text, "final warning");
    assert_eq!(warnings[0].report_id, Some(report_id.parse().unwrap()));
    assert_eq!(warnings[0].status_ids, vec![status_id.parse::<Id>().unwrap()]);
    // The warn-only action resolves its own report.
    let (status, _, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/admin/reports/{report_id}"), Some(&mod_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["action_taken"], true);

    // Warn without a report stays 422.
    let (status, _, _) = send(
        &srv.router,
        post_admin(&action_uri, "type=none", Some(&admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A warning preset id names a row of a model that does not exist → 404.
    let (status, _, _) = send(
        &srv.router,
        post_admin(
            &action_uri,
            &format!("type=none&report_id={report_id}&warning_preset_id=1"),
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Other action types record strikes too (upstream `process_strike!`).
    let report2 = file_report(&srv.router, &user_token, mod_id, "").await;
    let report2_id = report2["id"].as_str().unwrap().to_string();
    let (status, _, _) = send(
        &srv.router,
        post_admin(
            &action_uri,
            &format!("type=silence&report_id={report2_id}&text=quiet+please"),
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let warnings = srv.store.admin_warnings_for_account(mod_id).expect("warnings");
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().any(|w| w.action == "silence" && w.text == "quiet please"));

    let _ = user_id;
}

// ---------------------------------------------------------------------------
// D39: served vs refused metric keys.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d39_measures_served_and_refused() {
    let srv = TestServer::new(true);
    let app = register_app(&srv.router, ALL_SCOPES).await;
    let (admin_token, _) = register_account(&srv.router, &app, "boss").await;
    let (alice_token, _) = register_account(&srv.router, &app, "alice").await;
    let (bob_token, _) = register_account(&srv.router, &app, "bob").await;
    let account_id = |name: &str| {
        srv.store
            .account_by_handle(name, "example.social")
            .expect("lookup")
            .expect("account")
            .id
    };
    let admin_id = account_id("boss");
    let bob_id = account_id("bob");
    srv.store.set_user_role(admin_id, UserRole::Admin).expect("role");

    // Real activity: a follow, a resolved report, a password sign-in.
    let (status, _, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/follow"), "", Some(&alice_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let report = file_report(&srv.router, &alice_token, bob_id, "").await;
    let report_id = report["id"].as_str().unwrap().to_string();
    let (status, _, _) = send(
        &srv.router,
        post_admin(&format!("/api/v1/admin/reports/{report_id}/resolve"), "", Some(&admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Password login through the browser flow (records sign-in telemetry).
    let login_body = format!(
        "username={}&password=password123&client_id={}&redirect_uri={}&response_type=code&scope=read&state=x",
        enc("alice"),
        enc(app["client_id"].as_str().unwrap()),
        enc("https://client.example/cb"),
    );
    let (status, headers, _) = send(
        &srv.router,
        form_request("POST", "/oauth/authorize", &login_body, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(common::session_cookie(&headers).is_some(), "login must establish a session");

    // Served keys carry real values; refused keys stay omitted (never nulled
    // or fabricated), like upstream's `filter_map` drop.
    let body = "keys[]=new_users&keys[]=opened_reports&keys[]=resolved_reports&keys[]=active_users&keys[]=instance_follows&keys[]=instance_followers&keys[]=instance_statuses&keys[]=instance_reports&keys[]=instance_accounts&keys[]=instance_media_attachments&keys[]=interactions&keys[]=tag_accounts&keys[]=tag_uses&keys[]=tag_servers&keys[]=bogus_key";
    let (status, _, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/admin/measures", body, Some(&admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rows = json(&bytes);
    let by_key = |key: &str| {
        rows.as_array().unwrap().iter().find(|r| r["key"] == key).cloned()
    };
    for refused in ["interactions", "tag_accounts", "tag_uses", "tag_servers", "bogus_key"] {
        assert!(by_key(refused).is_none(), "{refused} must stay omitted");
    }
    let resolved = by_key("resolved_reports").expect("resolved_reports served");
    assert!(resolved["total"].as_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert!(resolved.get("previous_total").is_some(), "in-window totals carry previous_total");
    let active = by_key("active_users").expect("active_users served");
    assert!(active["total"].as_str().unwrap().parse::<u64>().unwrap() >= 1);
    let follows = by_key("instance_follows").expect("instance_follows served");
    assert_eq!(follows["total"], "1");
    assert!(follows.get("previous_total").is_none(), "all-time totals omit previous_total");
    let followers = by_key("instance_followers").expect("instance_followers served");
    assert_eq!(followers["total"], "1");
    assert!(followers.get("previous_total").is_none());
    assert!(by_key("new_users").is_some());
    assert!(by_key("opened_reports").is_some());

    // Dimensions: instance breakdowns served; version/tag censuses refused.
    let body = "keys[]=languages&keys[]=sources&keys[]=servers&keys[]=space_usage&keys[]=instance_accounts&keys[]=instance_languages&keys[]=software_versions&keys[]=tag_servers&keys[]=tag_languages";
    let (status, _, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/admin/dimensions", body, Some(&admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rows = json(&bytes);
    let by_key = |key: &str| {
        rows.as_array().unwrap().iter().find(|r| r["key"] == key).cloned()
    };
    for refused in ["software_versions", "tag_servers", "tag_languages"] {
        assert!(by_key(refused).is_none(), "{refused} must stay omitted");
    }
    let instance_accounts = by_key("instance_accounts").expect("instance_accounts served");
    let names: Vec<&str> = instance_accounts["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"bob"), "followed account tops the domain: {names:?}");
    assert!(by_key("instance_languages").is_some());
    assert!(by_key("space_usage").is_some());

    // Retention still answers (authorship-based cohorts until sign-in
    // cohorts land — documented deviation, not a refusal).
    let (status, _, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/admin/retention",
            "start_at=2020-01-01&end_at=2030-01-01&frequency=month",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(json(&bytes).as_array().is_some());

    let _ = bob_token;
}
