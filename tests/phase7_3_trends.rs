//! Phase 7.3 trends contract tests (Subagent D).
//!
//! Public reads (`/api/v1/trends`, `/tags`, `/links`, `/statuses`) serve
//! only admin-approved items with `offset`/`limit` paging (default 10) and
//! no scope requirement; the admin review queue approves/rejects candidates
//! derived from real usage, one audit record per write. Router wiring lives
//! with the main session (`router.rs` is frozen); these tests mount the same
//! handlers on a test router sharing one `ApiState`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get as route_get, post as route_post};

use birchtree::api::mastodon::endpoints::{admin_trends, trends};
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::context::Context;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::ports::{NewToken, Store};
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};
use common::{TestServer, federation_config, form_request, get_request, json, send, server_info, TEST_PUBLIC_URL};

/// A harness sharing one store between the main router (fixtures: apps,
/// accounts, statuses) and the trends/admin-trends handlers.
struct Harness {
    srv: TestServer,
    router: axum::Router,
}

fn api_state(srv: &TestServer, trends_enabled: bool) -> ApiState {
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
    let mut server: ServerInfo = (*server_info()).clone();
    server.trends_enabled = trends_enabled;
    let federation = federation_config(context.clone(), TEST_PUBLIC_URL);
    ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server: Arc::new(server),
        streams: streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
    }
}

fn trends_router(state: ApiState) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/trends", route_get(trends::trends_alias))
        .route("/api/v1/trends/tags", route_get(trends::tags_index))
        .route("/api/v1/trends/links", route_get(trends::links_index))
        .route("/api/v1/trends/statuses", route_get(trends::statuses_index))
        .route("/api/v1/admin/trends/tags", route_get(admin_trends::tags_index))
        .route(
            "/api/v1/admin/trends/tags/:id/approve",
            route_post(admin_trends::tags_approve),
        )
        .route(
            "/api/v1/admin/trends/tags/:id/reject",
            route_post(admin_trends::tags_reject),
        )
        .route("/api/v1/admin/trends/links", route_get(admin_trends::links_index))
        .route(
            "/api/v1/admin/trends/links/:id/approve",
            route_post(admin_trends::links_approve),
        )
        .route(
            "/api/v1/admin/trends/links/:id/reject",
            route_post(admin_trends::links_reject),
        )
        .route(
            "/api/v1/admin/trends/links/publishers",
            route_get(admin_trends::publishers_index),
        )
        .route(
            "/api/v1/admin/trends/links/publishers/:id/approve",
            route_post(admin_trends::publishers_approve),
        )
        .route(
            "/api/v1/admin/trends/links/publishers/:id/reject",
            route_post(admin_trends::publishers_reject),
        )
        .route(
            "/api/v1/admin/trends/statuses",
            route_get(admin_trends::statuses_index),
        )
        .route(
            "/api/v1/admin/trends/statuses/:id/approve",
            route_post(admin_trends::statuses_approve),
        )
        .route(
            "/api/v1/admin/trends/statuses/:id/reject",
            route_post(admin_trends::statuses_reject),
        )
        .with_state(state)
}

impl Harness {
    fn new() -> Self {
        Self::with_trends(true)
    }

    fn with_trends(trends_enabled: bool) -> Self {
        let srv = TestServer::new(true);
        let state = api_state(&srv, trends_enabled);
        let router = trends_router(state);
        Self { srv, router }
    }
}

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

async fn register_app(h: &Harness, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=Trends&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(h: &Harness, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(h: &Harness, app: &serde_json::Value, username: &str) -> (String, serde_json::Value) {
    let app_tok = app_token(h, app, "write:accounts").await;
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &h.srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let value = json(&bytes);
    (value["access_token"].as_str().unwrap().to_string(), value)
}

/// A user token minted directly from the store (bypasses app registration,
/// so tests can use scope sets the OAuth flow would not grant).
fn direct_token(h: &Harness, app_client_id: &str, user_id: Option<Id>, scopes: &[&str]) -> String {
    let store: &dyn Store = &*h.srv.store;
    let app = store.app_by_client_id(app_client_id).unwrap().expect("app");
    let plaintext = format!("trend-test-{}-{}", user_id.map(|i| i.to_string()).unwrap_or_else(|| "app".into()), scopes.join("+"));
    store
        .issue_access_token(NewToken {
            app_id: app.id,
            user_id,
            token_hash: sha256_hex(plaintext.as_bytes()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            now_ms: common::T0,
            expires_at_ms: None,
        })
        .unwrap();
    plaintext
}

async fn post_status(h: &Harness, token: &str, text: &str) -> serde_json::Value {
    let body = format!("status={}", enc(text));
    let (status, _h, bytes) = send(
        &h.srv.router,
        form_request("POST", "/api/v1/statuses", &body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "post: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn get(router: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let (status, headers, bytes) = send(router, get_request(uri, token, None)).await;
    (status, headers, json(&bytes))
}

async fn post_empty(router: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let (status, _h, bytes) = send(router, form_request("POST", uri, "", token)).await;
    (status, json(&bytes))
}

const USER_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses write:statuses read:blocks write:blocks write:mutes";

async fn admin_setup(h: &Harness) -> (String, String) {
    let app = register_app(h, &format!("{USER_SCOPES} admin:read admin:write")).await;
    let (admin_token, _) = register_account(h, &app, "moderator").await;
    let (user_token, _) = register_account(h, &app, "alice").await;
    let store: &dyn Store = &*h.srv.store;
    let admin_id = store
        .account_by_handle("moderator", "example.social")
        .unwrap()
        .expect("admin account")
        .id;
    store.set_user_role(admin_id, UserRole::Admin).unwrap();
    (admin_token, user_token)
}

// ----- public tag trends --------------------------------------------------------

#[tokio::test]
async fn unapproved_tags_are_invisible_approved_tags_are_public() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;

    // Nothing approved yet: anonymous and authenticated reads are empty.
    let (status, _, body) = get(&h.router, "/api/v1/trends/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    post_status(&h, &user_token, "hello #rust world").await;

    // Still invisible before approval (pending review is not public).
    let (status, _, body) = get(&h.router, "/api/v1/trends/tags", Some(&user_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    // The admin queue sees the pending candidate.
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/tags", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], serde_json::json!("rust"));
    assert_eq!(body[0]["requires_review"], serde_json::json!(true));
    assert_eq!(body[0]["trendable"], serde_json::Value::Null);
    let id = body[0]["id"].as_str().unwrap().to_string();

    // Approve: public reads (anonymous included) now serve it.
    let (status, body) = post_empty(&h.router, &format!("/api/v1/admin/trends/tags/{id}/approve"), Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["trendable"], serde_json::json!(true));
    assert_eq!(body["requires_review"], serde_json::json!(false));

    let (status, _, body) = get(&h.router, "/api/v1/trends/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], serde_json::json!("rust"));
    // Anonymous payloads carry no viewer keys.
    assert!(body[0].get("following").is_none());

    // Reject hides it again.
    let (status, _) = post_empty(&h.router, &format!("/api/v1/admin/trends/tags/{id}/reject"), Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = get(&h.router, "/api/v1/trends/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    // One audit record per write (approve + reject).
    let store: &dyn Store = &*h.srv.store;
    let trail = store.trend_audit_trail("trend_tag", &id).unwrap();
    assert_eq!(trail.len(), 2);
    assert!(trail.iter().all(|r| r.target_id == id));
}

#[tokio::test]
async fn trends_alias_serves_tags_with_deprecation_header() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;
    post_status(&h, &user_token, "post #deprecation").await;

    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/tags", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let id = body[0]["id"].as_str().unwrap().to_string();
    post_empty(&h.router, &format!("/api/v1/admin/trends/tags/{id}/approve"), Some(&admin_token)).await;

    let (status, headers, alias_body) = get(&h.router, "/api/v1/trends", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, tags_body) = get(&h.router, "/api/v1/trends/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alias_body, tags_body);
    // Upstream deprecated /api/v1/trends on 2022-03-30: `@<unix timestamp>`.
    assert_eq!(
        headers.get("Deprecation").map(|v| v.to_str().unwrap()),
        Some("@1648612800")
    );
}

#[tokio::test]
async fn tag_paging_is_offset_and_limit() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;
    post_status(&h, &user_token, "#t1 #t2 #t3 #t4 #t5").await;

    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/tags", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 5);
    for row in body.as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let (status, _) = post_empty(&h.router, &format!("/api/v1/admin/trends/tags/{id}/approve"), Some(&admin_token)).await;
        assert_eq!(status, StatusCode::OK);
    }

    // Equal usage sorts by name; offset=2, limit=2 picks t3, t4.
    let (status, headers, body) = get(&h.router, "/api/v1/trends/tags?offset=2&limit=2", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body.as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["t3", "t4"]);
    let link = headers.get("Link").unwrap().to_str().unwrap();
    assert!(link.contains("offset=4") && link.contains("rel=\"next\""), "{link}");
    assert!(!link.contains("rel=\"prev\""), "{link}");

    // Last partial page has a prev link and no next link.
    let (status, headers, body) = get(&h.router, "/api/v1/trends/tags?offset=4&limit=2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    let link = headers.get("Link").unwrap().to_str().unwrap();
    assert!(!link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("offset=2") && link.contains("rel=\"prev\""), "{link}");
}

#[tokio::test]
async fn public_trends_need_no_scope_but_reject_bad_tokens() {
    let h = Harness::new();
    let (status, _, body) = get(&h.router, "/api/v1/trends/tags", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
    // A present-but-invalid token is a 401 (authorize_if_got_token!).
    let (status, _, _) = get(&h.router, "/api/v1/trends/tags", Some("bogus")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nothing_served_when_trends_disabled() {
    let h = Harness::with_trends(false);
    let (admin_token, user_token) = admin_setup(&h).await;
    post_status(&h, &user_token, "post #offswitch").await;
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/tags", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let id = body[0]["id"].as_str().unwrap().to_string();
    post_empty(&h.router, &format!("/api/v1/admin/trends/tags/{id}/approve"), Some(&admin_token)).await;
    for path in ["/api/v1/trends/tags", "/api/v1/trends/links", "/api/v1/trends/statuses", "/api/v1/trends"] {
        let (status, _, body) = get(&h.router, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body, serde_json::json!([]), "{path}");
    }
}

// ----- link trends --------------------------------------------------------------

#[tokio::test]
async fn link_candidates_approve_to_public_cards_without_fabricated_metadata() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;
    post_status(&h, &user_token, "read https://example.com/article today").await;

    // Pending link visible in the admin queue, invisible publicly.
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/links", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["url"], serde_json::json!("https://example.com/article"));
    assert_eq!(body[0]["requires_review"], serde_json::json!(true));
    let id = body[0]["id"].as_str().unwrap().to_string();
    let (status, _, body) = get(&h.router, "/api/v1/trends/links", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    // The publisher host materialized alongside the link.
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/links/publishers", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let domains: Vec<&str> = body.as_array().unwrap().iter().map(|v| v["domain"].as_str().unwrap()).collect();
    assert!(domains.contains(&"example.com"), "{domains:?}");

    // Approve: the public card serves the real URL with null metadata
    // (no crawler yet — never fabricated).
    let (status, _body) = post_empty(&h.router, &format!("/api/v1/admin/trends/links/{id}/approve"), Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = get(&h.router, "/api/v1/trends/links", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["url"], serde_json::json!("https://example.com/article"));
    assert_eq!(body[0]["title"], serde_json::Value::Null);
    assert_eq!(body[0]["image"], serde_json::Value::Null);

    // Rejected links stay hidden.
    post_status(&h, &user_token, "also https://example.org/other").await;
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/links", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    let other = body.as_array().unwrap().iter().find(|v| v["url"] == serde_json::json!("https://example.org/other")).unwrap();
    let other_id = other["id"].as_str().unwrap().to_string();
    post_empty(&h.router, &format!("/api/v1/admin/trends/links/{other_id}/reject"), Some(&admin_token)).await;
    let (status, _, body) = get(&h.router, "/api/v1/trends/links", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ----- status trends ------------------------------------------------------------

#[tokio::test]
async fn status_trends_are_approved_and_viewer_gated() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;
    let posted = post_status(&h, &user_token, "trend-worthy #news").await;
    let status_id = posted["id"].as_str().unwrap().to_string();

    // Candidate appears in the admin queue as pending.
    let (status, _, body) = get(&h.router, "/api/v1/admin/trends/statuses", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["requires_review"], serde_json::json!(true));

    // Approve by status id: anonymous readers see it.
    let (status, body) = post_empty(&h.router, &format!("/api/v1/admin/trends/statuses/{status_id}/approve"), Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requires_review"], serde_json::json!(false));
    let (status, _, body) = get(&h.router, "/api/v1/trends/statuses", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], serde_json::json!(status_id));

    // Approving an unknown status is a 404, not a fabricated row.
    let (status, _) = post_empty(&h.router, "/api/v1/admin/trends/statuses/99999999999999999/approve", Some(&admin_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn private_approved_statuses_do_not_leak_to_anonymous() {
    let h = Harness::new();
    let (admin_token, user_token) = admin_setup(&h).await;
    let body = format!("status={}&visibility=private", enc("followers-only #news"));
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/api/v1/statuses", &body, Some(&user_token))).await;
    assert_eq!(status, StatusCode::OK);
    let status_id = json(&bytes)["id"].as_str().unwrap().to_string();

    let (status, _) = post_empty(&h.router, &format!("/api/v1/admin/trends/statuses/{status_id}/approve"), Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);

    // Anonymous: nothing. The author: their own status.
    let (status, _, body) = get(&h.router, "/api/v1/trends/statuses", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
    let (status, _, body) = get(&h.router, "/api/v1/trends/statuses", Some(&user_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn muted_authors_are_filtered_for_the_muter() {
    let h = Harness::new();
    let app = register_app(&h, &format!("{USER_SCOPES} admin:read admin:write")).await;
    let (admin_token, _) = register_account(&h, &app, "root").await;
    let (alice_token, _) = register_account(&h, &app, "alice").await;
    let (bob_token, _) = register_account(&h, &app, "bob").await;
    let bob_id = {
        let store: &dyn Store = &*h.srv.store;
        store.account_by_handle("bob", "example.social").unwrap().expect("bob").id.to_string()
    };
    {
        let store: &dyn Store = &*h.srv.store;
        let root = store.account_by_handle("root", "example.social").unwrap().expect("root");
        store.set_user_role(root.id, UserRole::Admin).unwrap();
    }

    let posted = post_status(&h, &bob_token, "bob says #news").await;
    let status_id = posted["id"].as_str().unwrap().to_string();
    post_empty(&h.router, &format!("/api/v1/admin/trends/statuses/{status_id}/approve"), Some(&admin_token)).await;

    // Alice mutes Bob: his approved status vanishes for her only.
    let (status, _, _) = send(
        &h.srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/mute"), "", Some(&alice_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = get(&h.router, "/api/v1/trends/statuses", Some(&alice_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
    let (status, _, body) = get(&h.router, "/api/v1/trends/statuses", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ----- admin auth matrix ----------------------------------------------------------

#[tokio::test]
async fn admin_trends_require_admin_role_before_scope() {
    let h = Harness::new();
    let app = register_app(&h, &format!("{USER_SCOPES} admin:read admin:write")).await;
    let (plain_token, _) = register_account(&h, &app, "plain").await;
    let plain_id: Id = {
        let store: &dyn Store = &*h.srv.store;
        store.account_by_handle("plain", "example.social").unwrap().expect("plain").id
    };

    // No token: 401.
    let (status, _) = post_empty(&h.router, "/api/v1/admin/trends/tags/1/approve", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Role-less user with broad scopes: 403 Forbidden (role before scope).
    let (status, body) = post_empty(&h.router, "/api/v1/admin/trends/tags/1/approve", Some(&plain_token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], serde_json::json!("Forbidden"));
    // Admin user whose token lacks the write scope: 403 scopes.
    {
        let store: &dyn Store = &*h.srv.store;
        let admin = store.account_by_handle("plain", "example.social").unwrap().expect("plain");
        store.set_user_role(admin.id, UserRole::Admin).unwrap();
    }
    let scoped = direct_token(&h, app["client_id"].as_str().unwrap(), Some(plain_id), &["admin:read"]);
    let (status, body) = post_empty(&h.router, "/api/v1/admin/trends/tags/1/approve", Some(&scoped)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], serde_json::json!("This action is outside the authorized scopes"));
    // App-only token (no user): 422.
    let app_only = app_token(&h, &app, "admin:write").await;
    let (status, body) = post_empty(&h.router, "/api/v1/admin/trends/tags/1/approve", Some(&app_only)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], serde_json::json!("This method requires an authenticated user"));
}
