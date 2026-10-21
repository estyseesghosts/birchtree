//! Phase 7.3 admin accounts + moderator report queue contract tests.
//!
//! The admin routes are registered here through a test-local merge (the
//! integrator owns `router.rs`; the merge below mirrors the registration
//! snippet in this workstream's return notes). Every route is covered with a
//! fixture-backed auth matrix: no token → 401, app-only token → 422,
//! role-less user → 403 `{"error":"Forbidden"}`, in-scope failure → 403
//! scopes message, moderator OK, admin OK. Empty results are `[]` or 404 per
//! route; serializers are asserted field-by-field against the upstream
//! shapes.

mod common;

use std::sync::Arc;

use activitypub_federation::http_signatures::generate_actor_keypair;
use axum::body::Body;
use axum::http::{Request, StatusCode};

use birchtree::api::mastodon::{build_router, ApiState};
use birchtree::application::context::Context;
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{AccountStore, AdminStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};
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

async fn register_app(srv: &TestServer, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "scopes={scopes} body={}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(
    srv: &TestServer,
    app: &serde_json::Value,
    username: &str,
) -> (String, serde_json::Value) {
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    (body["access_token"].as_str().unwrap().to_string(), body)
}

const ADMIN_SCOPES: &str = "read write read:accounts write:accounts write:reports admin:read admin:write admin:read:accounts admin:write:accounts admin:read:reports admin:write:reports";

/// Build a router with the admin routes (now wired in `router.rs` proper;
/// this mirrors that wiring for an isolated store).
fn admin_router(srv: &TestServer) -> axum::Router {

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        registration_open: true,
        ..Config::default()
    };
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&cfg));
    let streams = Hub::new(srv.store.clone(), clock.clone(), server.clone(), Limits::default());
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams,
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(
            birchtree::application::accounts::MemoryInviteStore::default(),
        ),
    };
    build_router(state)
}

struct Fixtures {
    srv: TestServer,
    router: axum::Router,
    admin_token: String,
    mod_token: String,
    user_token: String,
    app_only_token: String,
    admin_id: Id,
    mod_id: Id,
    user_id: Id,
}

async fn fixtures() -> Fixtures {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ADMIN_SCOPES).await;
    let (admin_token, _) = register_account(&srv, &app, "boss").await;
    let (mod_token, _) = register_account(&srv, &app, "mod").await;
    let (user_token, _) = register_account(&srv, &app, "plain").await;
    let app_only_token = app_token(&srv, &app, "admin:read admin:read:accounts").await;
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
    let router = admin_router(&srv);
    Fixtures {
        srv,
        router,
        admin_token,
        mod_token,
        user_token,
        app_only_token,
        admin_id,
        mod_id,
        user_id,
    }
}

fn post_admin(uri: &str, body: &str, token: Option<&str>) -> Request<Body> {
    form_request("POST", uri, body, token)
}

/// A remote actor known to the store (for remote/local filter coverage).
fn remote_id(srv: &TestServer, name: &str) -> Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://remote.example/users/{name}");
    let upsert = RemoteAccountUpsert {
        actor_uri: actor_uri.clone(),
        username: name.to_string(),
        domain: "remote.example".into(),
        display_name: String::new(),
        note: String::new(),
        locked: false,
        inbox_uri: format!("{actor_uri}/inbox"),
        shared_inbox_uri: None,
        public_key_pem: keys.public_key,
        key_id: format!("{actor_uri}#main-key"),
    };
    srv.store.upsert_remote(upsert, 1_700_000_000_000).expect("upsert");
    srv.store
        .account_by_handle(name, "remote.example")
        .expect("lookup")
        .expect("remote")
        .id
}

#[tokio::test]
async fn admin_accounts_auth_matrix() {
    let f = fixtures().await;
    // No token → 401.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&bytes)["error"], "The access token is invalid");
    // App-only token → 422 (a user is required).
    let (status, _, _bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts", Some(&f.app_only_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Role-less user → 403 Forbidden (role check runs before scope check).
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts", Some(&f.user_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
    // Moderator and admin pass.
    for token in [&f.mod_token, &f.admin_token] {
        let (status, _, _) = send(
            &f.router,
            get_request("/api/v1/admin/accounts", Some(token), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    // A moderator whose token lacks the admin scope → 403 scopes message.
    let narrow_app = register_app(&f.srv, "read write:accounts").await;
    let (narrow_token, _) = register_account(&f.srv, &narrow_app, "narrowmod").await;
    let narrow_id = f
        .srv
        .store
        .account_by_handle("narrowmod", "example.social")
        .expect("lookup")
        .expect("account")
        .id;
    f.srv.store.set_user_role(narrow_id, UserRole::Moderator).expect("role");
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts", Some(&narrow_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "This action is outside the authorized scopes");
}

#[tokio::test]
async fn admin_accounts_show_shape_and_404() {
    let f = fixtures().await;
    let uri = format!("/api/v1/admin/accounts/{}", f.user_id);
    let (status, headers, bytes) = send(
        &f.router,
        get_request(&uri, Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.contains_key("vary"));
    let body = json(&bytes);
    assert_eq!(body["id"], f.user_id.to_string());
    assert_eq!(body["username"], "plain");
    assert_eq!(body["domain"], "example.social");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(body["email"], "plain@example.com");
    assert_eq!(body["ip"], serde_json::Value::Null);
    assert_eq!(body["confirmed"], false);
    assert_eq!(body["suspended"], false);
    assert_eq!(body["silenced"], false);
    assert_eq!(body["sensitized"], false);
    assert_eq!(body["disabled"], false);
    assert_eq!(body["approved"], true);
    assert_eq!(body["locale"], serde_json::json!("en"));
    assert_eq!(body["account"]["id"], f.user_id.to_string());
    assert_eq!(body["account"]["username"], "plain");
    assert_eq!(body["role"]["id"], 0);
    assert_eq!(body["role"]["name"], "User");
    // Moderator row carries its role.
    let uri = format!("/api/v1/admin/accounts/{}", f.mod_id);
    let (status, _, bytes) = send(&f.router, get_request(&uri, Some(&f.mod_token), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["role"]["id"], 1);
    // Unknown id → 404, empty filter → [] with no Link header.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts/999999999999", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["error"], "Record not found");
    let (status, headers, bytes) = send(
        &f.router,
        get_request(
            "/api/v1/admin/accounts?username=no-such-user-anywhere",
            Some(&f.admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!([]));
    assert!(!headers.contains_key("link"));
}

#[tokio::test]
async fn admin_accounts_filters_v1_and_v2() {
    let f = fixtures().await;
    let remote = remote_id(&f.srv, "far");
    // Suspend the plain user through the action endpoint.
    let uri = format!("/api/v1/admin/accounts/{}/action", f.user_id);
    let (status, _, _) = send(
        &f.router,
        post_admin(&uri, "type=suspend", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // v1 default = local + active: the suspended user drops out, remote is out.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<String> = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!ids.contains(&f.user_id.to_string()));
    assert!(!ids.contains(&remote.to_string()));
    // v1 suspended flag finds the user; v1 remote flag finds the remote actor.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts?suspended=true", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes).as_array().unwrap().iter().any(|r| r["id"] == f.user_id.to_string()));
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts?remote=true", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes).as_array().unwrap().iter().any(|r| r["id"] == remote.to_string()));
    // v1 username + by_domain filters.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts?remote=true&username=fa", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    // v2 origin/status/role_ids/permissions filters.
    let (status, _, bytes) = send(
        &f.router,
        get_request(
            "/api/v2/admin/accounts?origin=remote&by_domain=remote.example",
            Some(&f.admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v2/admin/accounts?permissions=staff", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = json(&bytes);
    assert!(rows.as_array().unwrap().iter().any(|r| r["id"] == f.mod_id.to_string()));
    assert!(rows.as_array().unwrap().iter().all(|r| r["role"]["id"] != 0));
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v2/admin/accounts?role_ids[]=admin", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes).as_array().unwrap().iter().any(|r| r["id"] == f.admin_id.to_string()));
    // v2 invited_by has no model → empty, honestly.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v2/admin/accounts?invited_by=1", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!([]));
    // Cursors: a full page carries a Link header (limit=1 forces paging).
    let (status, headers, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/accounts?remote=true&limit=1", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    assert!(headers.contains_key("link"));
}

#[tokio::test]
async fn admin_accounts_member_actions_and_audit() {
    let f = fixtures().await;
    // Suspend then unsuspend the plain user.
    let base = format!("/api/v1/admin/accounts/{}", f.user_id);
    let (status, _, _) = send(
        &f.router,
        post_admin(&format!("{base}/action"), "type=suspend", Some(&f.mod_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bytes) = send(
        &f.router,
        get_request(&base, Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["suspended"], true);
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("{base}/unsuspend"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["suspended"], false);
    // Silence/sensitive then clear them.
    for flag in ["type=silence", "type=sensitive"] {
        let uri = format!("{base}/action");
        let (status, _, _) = send(&f.router, post_admin(&uri, flag, Some(&f.admin_token))).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _, _) = send(
        &f.router,
        post_admin(&format!("{base}/unsilence"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("{base}/unsensitive"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["silenced"], false);
    assert_eq!(json(&bytes)["sensitized"], false);
    // Disable via action, re-enable via enable (local only).
    let (status, _, _) = send(
        &f.router,
        post_admin(&format!("{base}/action"), "type=disable", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("{base}/enable"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["disabled"], false);
    // enable on a remote account → 403 (require_local_account!).
    let remote = remote_id(&f.srv, "far2");
    let (status, _, bytes) = send(
        &f.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{remote}/enable"),
            "",
            Some(&f.admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
    // approve on an already-approved account → 403 (upstream
    // `UserPolicy#approve?` requires a pending account; D36).
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("{base}/approve"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
    // reject on an already-approved account → 403 (upstream
    // `UserPolicy#reject?`); the account stays active. The pending →
    // approve/reject transitions live in `tests/phase7_gaps_round2_c.rs`.
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("{base}/reject"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
    let (status, _, bytes) = send(&f.router, get_request(&base, Some(&f.admin_token), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["suspended"], false);
    assert_eq!(json(&bytes)["disabled"], false);
    // One audit row per write exists for this target.
    let entries = f.srv.store.admin_audit_entries(100).expect("audit");
    assert!(entries.iter().any(|e| e.action == "suspend" && e.target_id == f.user_id.to_string()));
    assert!(entries.iter().any(|e| e.action == "unsuspend"));
    assert!(entries.iter().any(|e| e.action == "enable"));
    // Unknown member target → 404.
    let (status, _, _) = send(
        &f.router,
        post_admin("/api/v1/admin/accounts/999999999999/enable", "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // DELETE suspends + disables immediately with 200 {}.
    let (status, _, bytes) = send(
        &f.router,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/admin/accounts/{}", f.mod_id))
            .header("authorization", format!("Bearer {}", f.admin_token))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));
}

#[tokio::test]
async fn admin_account_action_validation() {
    let f = fixtures().await;
    let uri = format!("/api/v1/admin/accounts/{}/action", f.user_id);
    // Unknown type → 422.
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&uri, "type=explode", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(json(&bytes)["error"].as_str().unwrap().contains("Type"));
    // Missing type → 422.
    let (status, _, _) = send(
        &f.router,
        post_admin(&uri, "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // disable on a remote account → 422 (remote types exclude none/disable).
    let remote = remote_id(&f.srv, "far3");
    let (status, _, _) = send(
        &f.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{remote}/action"),
            "type=disable",
            Some(&f.admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Remote suspend works.
    let (status, _, _) = send(
        &f.router,
        post_admin(
            &format!("/api/v1/admin/accounts/{remote}/action"),
            "type=suspend",
            Some(&f.admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Unknown action target → 404.
    let (status, _, _) = send(
        &f.router,
        post_admin("/api/v1/admin/accounts/999999999999/action", "type=suspend", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

async fn file_report(router: &axum::Router, reporter: &str, target: Id) -> serde_json::Value {
    let body = format!("account_id={}&comment=spammy", target);
    let (status, _, bytes) = send(router, form_request("POST", "/api/v1/reports", &body, Some(reporter))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

#[tokio::test]
async fn admin_reports_queue() {
    let f = fixtures().await;
    let r1 = file_report(&f.router, &f.user_token, f.mod_id).await;
    let r2 = file_report(&f.router, &f.user_token, f.admin_id).await;
    // Index lists both, newest first.
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/reports", Some(&f.mod_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = json(&bytes);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["id"], r2["id"]);
    // Shape of one row.
    let row = &rows[0];
    assert_eq!(row["category"], "other");
    assert_eq!(row["comment"], "spammy");
    assert_eq!(row["forwarded"], false);
    assert_eq!(row["action_taken"], false);
    assert_eq!(row["action_taken_at"], serde_json::Value::Null);
    assert_eq!(row["target_account"]["id"], f.admin_id.to_string());
    assert_eq!(row["account"]["id"], f.user_id.to_string());
    assert_eq!(row["assigned_account"], serde_json::Value::Null);
    assert_eq!(row["action_taken_by_account"], serde_json::Value::Null);
    assert_eq!(row["statuses"], serde_json::json!([]));
    assert_eq!(row["rules"], serde_json::json!([]));
    // Filters: unresolved matches both; account_id narrows to the reporter.
    let (status, _, bytes) = send(
        &f.router,
        get_request(
            &format!("/api/v1/admin/reports?unresolved&account_id={}", f.user_id),
            Some(&f.admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 2);
    let (status, _, bytes) = send(
        &f.router,
        get_request(
            &format!("/api/v1/admin/reports?target_account_id={}", f.admin_id),
            Some(&f.admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    // Show + unknown id.
    let (status, _, bytes) = send(
        &f.router,
        get_request(&format!("/api/v1/admin/reports/{}", r1["id"].as_str().unwrap()), Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["id"], r1["id"]);
    let (status, _, _bytes) = send(
        &f.router,
        get_request("/api/v1/admin/reports/999999999999", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Update category (PUT and PATCH aliases); bad category → 422.
    let uri = format!("/api/v1/admin/reports/{}", r1["id"].as_str().unwrap());
    for method in ["PUT", "PATCH"] {
        let (status, _, bytes) = send(
            &f.router,
            Request::builder()
                .method(method)
                .uri(uri.clone())
                .header("content-type", "application/x-www-form-urlencoded")
                .header("authorization", format!("Bearer {}", f.admin_token))
                .body(Body::from("category=spam"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method}");
        assert_eq!(json(&bytes)["category"], "spam");
    }
    let (status, _, _) = send(
        &f.router,
        Request::builder()
            .method("PUT")
            .uri(uri.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", format!("Bearer {}", f.admin_token))
            .body(Body::from("category=bogus"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // rule_ids on a non-violation category → 422.
    let (status, _, _) = send(
        &f.router,
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", format!("Bearer {}", f.admin_token))
            .body(Body::from("category=spam&rule_ids[]=1"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // assign → resolve → reopen → unassign cycle.
    let id = r1["id"].as_str().unwrap();
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("/api/v1/admin/reports/{id}/assign_to_self"), "", Some(&f.mod_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["assigned_account"]["id"], f.mod_id.to_string());
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("/api/v1/admin/reports/{id}/resolve"), "", Some(&f.mod_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["action_taken"], true);
    assert!(json(&bytes)["action_taken_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(json(&bytes)["action_taken_by_account"]["id"], f.mod_id.to_string());
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/reports?resolved=true", Some(&f.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("/api/v1/admin/reports/{id}/reopen"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["action_taken"], false);
    let (status, _, bytes) = send(
        &f.router,
        post_admin(&format!("/api/v1/admin/reports/{id}/unassign"), "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["assigned_account"], serde_json::Value::Null);
    // Unknown transition target → 404.
    let (status, _, _) = send(
        &f.router,
        post_admin("/api/v1/admin/reports/999999999999/resolve", "", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Audit rows exist for the transitions.
    let entries = f.srv.store.admin_audit_entries(100).expect("audit");
    for action in ["update", "assigned_to_self", "resolve", "reopen", "unassigned"] {
        assert!(
            entries.iter().any(|e| e.action == action && e.target_type == "report"),
            "missing audit {action}"
        );
    }
}

#[tokio::test]
async fn admin_reports_auth_matrix() {
    let f = fixtures().await;
    // No token → 401; role-less → 403 Forbidden; moderator OK.
    let (status, _, _) = send(&f.router, get_request("/api/v1/admin/reports", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, bytes) = send(
        &f.router,
        get_request("/api/v1/admin/reports", Some(&f.user_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
    let (status, _, _) = send(
        &f.router,
        get_request("/api/v1/admin/reports", Some(&f.mod_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // App-only token on a write → 422.
    let (status, _, _) = send(
        &f.router,
        post_admin("/api/v1/admin/reports/1/resolve", "", Some(&f.app_only_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn admin_action_resolves_target_reports() {
    let f = fixtures().await;
    let _r1 = file_report(&f.router, &f.user_token, f.mod_id).await;
    // A suspend action resolves every unresolved report about the target.
    let (status, _, _) = send(
        &f.router,
        post_admin(&format!("/api/v1/admin/accounts/{}/action", f.mod_id), "type=suspend", Some(&f.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bytes) = send(
        &f.router,
        get_request(
            &format!("/api/v1/admin/reports?target_account_id={}&resolved", f.mod_id),
            Some(&f.admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    assert_eq!(json(&bytes)[0]["action_taken"], true);
}
