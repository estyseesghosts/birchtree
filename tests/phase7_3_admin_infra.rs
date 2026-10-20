//! Phase 7.3 admin infrastructure contract tests (subagent C).
//!
//! Fixture-backed per route: domain allows/blocks, email-domain blocks, IP
//! blocks (+ request-path enforcement through the trusted-proxy client IP),
//! canonical email blocks (including the read-scoped `test` action), admin
//! tags, and the read-scoped measures/dimensions/retention analytics.
//!
//! Auth matrix per route family: 401 without a token, 422 with an app-only
//! token, 403 `Forbidden` for role-less and moderator callers (role before
//! scope), 403 scope errors for under-scoped admin tokens. Empty tables read
//! as `[]`; unknown ids read as 404 `Record not found`.
//!
//! The admin routes are mounted here on a test router built from the same
//! `ApiState` shape as production (`router.rs` itself is owned by the
//! integrator; see `admin_infra::routes` for the mount snippet).

mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::middleware;
use serde_json::Value;

use birchtree::api::mastodon::endpoints::admin_infra;
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::context::Context;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::ports::{
    AccountStore, AdminInfraStore, NewApp, NewToken, OAuthStore,
};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};

use common::{TEST_PUBLIC_URL, TestServer, form_request, get_request, json, send};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const ADMIN_SCOPES: &str = "admin:read admin:write admin:read:domain_allows admin:write:domain_allows admin:read:domain_blocks admin:write:domain_blocks admin:read:email_domain_blocks admin:write:email_domain_blocks admin:read:ip_blocks admin:write:ip_blocks admin:read:canonical_email_blocks admin:write:canonical_email_blocks";

/// An admin-routes router sharing the server's store, blobs and streams.
fn admin_router(srv: &TestServer, trusted_proxies: Vec<IpAddr>) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let cfg = Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(srv.streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(trusted_proxies),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: srv.streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(
            birchtree::application::accounts::MemoryInviteStore::default(),
        ),
    };
    admin_infra::routes().with_state(state)
}

fn register_app(srv: &TestServer) -> Id {
    srv.store
        .register_app(NewApp {
            name: "admin-infra-tests".to_string(),
            client_id: "admin-infra-client".to_string(),
            client_secret_hash: None,
            redirect_uris: Vec::new(),
            scopes: vec!["read".to_string(), "write".to_string()],
            website: None,
            now_ms: now_ms(),
        })
        .expect("register app")
        .id
}

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

/// Mint a token directly through the store (bypasses scope validation, so
/// admin scopes can be tested without the OAuth round-trip).
fn mint_token(srv: &TestServer, app_id: Id, user: Option<Id>, scopes: &str, tag: &str) -> String {
    let plaintext = format!("adm-infra-{tag}-{}", scopes.replace([' ', ':'], "-"));
    let token_hash = sha256_hex(plaintext.as_bytes());
    srv.store
        .issue_access_token(NewToken {
            app_id,
            user_id: user,
            token_hash,
            scopes: scopes.split_whitespace().map(str::to_string).collect(),
            now_ms: now_ms(),
            expires_at_ms: None,
        })
        .expect("issue token");
    plaintext
}

struct Fixtures {
    app_id: Id,
    admin: String,
    admin_read: String,
    admin_write_only: String,
    moderator: String,
    user: String,
    app_only: String,
}

fn fixtures(srv: &TestServer) -> Fixtures {
    let app_id = register_app(srv);
    let mk = |username: &str| -> Id {
        create_account(
            &*srv.store,
            TEST_PUBLIC_URL,
            now_ms(),
            CreateAccountCommand {
                username: username.to_string(),
                email: None,
                password: "password123".to_string(),
                display_name: None,
            },
        )
        .expect("create account")
        .id
    };
    let admin_id = mk("admin");
    let reader_id = mk("reader");
    let writer_id = mk("writer");
    let mod_id = mk("mod");
    let user_id = mk("user");
    srv.store.set_user_role(admin_id, UserRole::Admin).expect("grant admin");
    srv.store.set_user_role(reader_id, UserRole::Admin).expect("grant admin");
    srv.store.set_user_role(writer_id, UserRole::Admin).expect("grant admin");
    srv.store.set_user_role(mod_id, UserRole::Moderator).expect("grant moderator");
    Fixtures {
        app_id,
        admin: mint_token(srv, app_id, Some(admin_id), ADMIN_SCOPES, "admin"),
        admin_read: mint_token(srv, app_id, Some(reader_id), "admin:read", "reader"),
        admin_write_only: mint_token(srv, app_id, Some(writer_id), "admin:write", "writer"),
        moderator: mint_token(srv, app_id, Some(mod_id), ADMIN_SCOPES, "mod"),
        user: mint_token(srv, app_id, Some(user_id), ADMIN_SCOPES, "user"),
        app_only: mint_token(srv, app_id, None, ADMIN_SCOPES, "app"),
    }
}

async fn get(router: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(router, get_request(uri, token, None)).await;
    (status, json(&bytes))
}

async fn post(router: &axum::Router, uri: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(router, form_request("POST", uri, body, Some(token))).await;
    (status, json(&bytes))
}

async fn put(router: &axum::Router, uri: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(router, form_request("PUT", uri, body, Some(token))).await;
    (status, json(&bytes))
}

async fn patch(router: &axum::Router, uri: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(router, form_request("PATCH", uri, body, Some(token))).await;
    (status, json(&bytes))
}

async fn delete(router: &axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(router, form_request("DELETE", uri, "", Some(token))).await;
    (status, json(&bytes))
}

// ---------------------------------------------------------------------------
// Auth matrix.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_auth_matrix_role_before_scope() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);
    let uri = "/api/v1/admin/domain_allows";

    // No token → 401.
    let (status, body) = get(&router, uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "The access token is invalid");

    // App-only token → 422 (a user is required).
    let (status, body) = get(&router, uri, Some(fx.app_only.as_str())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "This method requires an authenticated user");

    // Role-less user with full scopes → 403 Forbidden (role before scope).
    let (status, body) = get(&router, uri, Some(fx.user.as_str())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "Forbidden");

    // Moderator with full scopes → 403 Forbidden (admin-only area).
    let (status, body) = get(&router, uri, Some(fx.moderator.as_str())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "Forbidden");

    // Admin with the bare read scope → 200 (`admin:read` alone satisfies the
    // any-of scope list).
    let (status, _body) = get(&router, uri, Some(fx.admin_read.as_str())).await;
    assert_eq!(status, StatusCode::OK);

    // Admin with an unrelated scope → 403 scope error.
    let unrelated = mint_token(&srv, fx.app_id, Some(admin_id_of(&srv, "reader")), "read", "unrelated");
    let (status, body) = get(&router, uri, Some(unrelated.as_str())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is outside the authorized scopes");

    // Admin with the sub-scope → 200, empty table reads as [].
    let (status, body) = get(&router, uri, Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));
}

fn admin_id_of(srv: &TestServer, username: &str) -> Id {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
}

// ---------------------------------------------------------------------------
// Domain allows.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_domain_allows_crud() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    // Unknown id → 404 Record not found.
    let (status, body) = get(&router, "/api/v1/admin/domain_allows/999991", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Record not found");
    // Unparsable id → 404 as well.
    let (status, _) = get(&router, "/api/v1/admin/domain_allows/abc", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Blank / malformed domains → pinned 422 messages.
    let (status, body) = post(&router, "/api/v1/admin/domain_allows", &fx.admin, "domain=").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Domain can't be blank");
    let (status, body) = post(&router, "/api/v1/admin/domain_allows", &fx.admin, "domain=not+a+domain").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Domain is invalid");

    // Create (mixed case + trailing dot normalize).
    let (status, body) = post(&router, "/api/v1/admin/domain_allows", &fx.admin, "domain=Example.COM.").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["domain"], "example.com");
    let id = body["id"].as_str().expect("id").to_string();
    assert!(body["created_at"].is_string());

    // Duplicate create returns the existing row (no duplicate).
    let (status, dupe) = post(&router, "/api/v1/admin/domain_allows", &fx.admin, "domain=example.com").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dupe["id"], id);

    // Show + index.
    let (status, body) = get(&router, &format!("/api/v1/admin/domain_allows/{id}"), Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["domain"], "example.com");
    let (status, body) = get(&router, "/api/v1/admin/domain_allows", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().expect("array").len(), 1);

    // Delete → 200 {}; second delete → 404.
    let (status, _) = delete(&router, &format!("/api/v1/admin/domain_allows/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = delete(&router, &format!("/api/v1/admin/domain_allows/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // One audit record per write (2 creates + 1 destroy).
    let log = srv.store.admin_audit_log(10).expect("audit log");
    assert_eq!(log.len(), 3);
    assert!(log.iter().all(|record| record.target_type == "DomainAllow"));
}

// ---------------------------------------------------------------------------
// Domain blocks.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_domain_blocks_crud() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    let (status, _) = get(&router, "/api/v1/admin/domain_blocks/42", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = post(
        &router,
        "/api/v1/admin/domain_blocks",
        &fx.admin,
        "domain=evil.example&severity=suspend&reject_media=true&private_comment=spam",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["domain"], "evil.example");
    assert_eq!(body["severity"], "suspend");
    assert_eq!(body["reject_media"], true);
    assert_eq!(body["reject_reports"], false);
    assert_eq!(body["private_comment"], "spam");
    assert_eq!(body["public_comment"], Value::Null);
    assert_eq!(body["obfuscate"], false);
    assert!(body["digest"].is_string());
    let id = body["id"].as_str().expect("id").to_string();

    // Duplicate → 422.
    let (status, body) = post(&router, "/api/v1/admin/domain_blocks", &fx.admin, "domain=evil.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Domain has already been taken");

    // Update severity (PUT) + alias (PATCH).
    let (status, body) = put(&router, &format!("/api/v1/admin/domain_blocks/{id}"), &fx.admin, "severity=noop").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["severity"], "noop");
    let (status, body) = patch(&router, &format!("/api/v1/admin/domain_blocks/{id}"), &fx.admin, "obfuscate=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["obfuscate"], true);

    // Update unknown → 404.
    let (status, _) = put(&router, "/api/v1/admin/domain_blocks/424242", &fx.admin, "severity=silence").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = delete(&router, &format!("/api/v1/admin/domain_blocks/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Email-domain blocks.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_email_domain_blocks_crud() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    let (status, body) = get(&router, "/api/v1/admin/email_domain_blocks", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    let (status, body) = post(
        &router,
        "/api/v1/admin/email_domain_blocks",
        &fx.admin,
        "domain=Spam.EXAMPLE&allow_with_approval=true",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["domain"], "spam.example");
    assert_eq!(body["allow_with_approval"], true);
    assert_eq!(body["history"], Value::Array(Vec::new()));
    let id = body["id"].as_str().expect("id").to_string();

    let (status, _) = post(&router, "/api/v1/admin/email_domain_blocks", &fx.admin, "domain=spam.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&router, "/api/v1/admin/email_domain_blocks", &fx.admin, "domain=").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = get(&router, &format!("/api/v1/admin/email_domain_blocks/{id}"), Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["domain"], "spam.example");

    let (status, _) = delete(&router, &format!("/api/v1/admin/email_domain_blocks/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// IP blocks (+ enforcement).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_ip_blocks_crud_and_validation() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    let (status, body) = get(&router, "/api/v1/admin/ip_blocks", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    // Blank severity → its own message (before the enum check).
    let (status, body) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=10.0.0.1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Severity can't be blank");
    // Unknown severity → enum message.
    let (status, body) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=10.0.0.1&severity=bogus").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Severity is not included in the list");
    // Bad IP → 422.
    let (status, _) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=999.1.1.1&severity=no_access").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Blank IP → 422.
    let (status, body) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=&severity=no_access").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Validation failed: Ip can't be blank");

    let (status, body) = post(
        &router,
        "/api/v1/admin/ip_blocks",
        &fx.admin,
        "ip=203.0.113.0%2F24&severity=no_access&comment=bad+net&expires_in=3600",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ip"], "203.0.113.0/24");
    assert_eq!(body["severity"], "no_access");
    assert_eq!(body["comment"], "bad net");
    assert!(body["expires_at"].is_string());
    let id = body["id"].as_str().expect("id").to_string();

    // Bare address normalizes to a host route.
    let (status, body) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=10.1.2.3&severity=sign_up_block").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ip"], "10.1.2.3/32");
    assert_eq!(body["expires_at"], Value::Null);

    // Duplicate → 422.
    let (status, _) = post(&router, "/api/v1/admin/ip_blocks", &fx.admin, "ip=203.0.113.0%2F24&severity=no_access").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Update severity; invalid severity → 422.
    let (status, body) = put(&router, &format!("/api/v1/admin/ip_blocks/{id}"), &fx.admin, "severity=sign_up_block").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["severity"], "sign_up_block");
    let (status, _) = put(&router, &format!("/api/v1/admin/ip_blocks/{id}"), &fx.admin, "severity=bogus").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = delete(&router, &format!("/api/v1/admin/ip_blocks/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::OK);
}

/// `no_access` blocks refuse the request in the middleware; `sign_up_block`
/// does not (it gates registration instead). The peer arrives as
/// `ConnectInfo<SocketAddr>`; behind a trusted proxy the leftmost
/// `X-Forwarded-For` entry wins.
#[tokio::test]
async fn admin_ip_block_enforcement() {
    let srv = TestServer::new(false);
    let proxy: IpAddr = "10.0.0.1".parse().unwrap();
    let router = admin_router(&srv, vec![proxy]);
    let fx = fixtures(&srv);

    // A probe route behind the enforcement middleware.
    let (probe, probe_state) = probe_parts(&srv, vec![proxy]);

    // Block the TEST-NET-3 range with no_access.
    let (status, _) = post(
        &router,
        "/api/v1/admin/ip_blocks",
        &fx.admin,
        "ip=203.0.113.0%2F24&severity=no_access",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Direct peer inside the range → refused.
    let (status, _h, bytes) = send(
        &probe,
        request_with_peer("/probe", "203.0.113.7:1234", None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");

    // X-Forwarded-For behind a trusted proxy → the forwarded client is checked.
    let (status, _, _) = send(
        &probe,
        request_with_peer("/probe", "10.0.0.1:5678", Some("203.0.113.77")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The same header from an untrusted peer is ignored → passes.
    let (status, _h, bytes) = send(
        &probe,
        request_with_peer("/probe", "198.51.100.9:9999", Some("203.0.113.77")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..], b"ok");

    // A `sign_up_block` row does not refuse requests (registration gate only).
    let blocks = srv.store.admin_ip_blocks_all(10).expect("blocks");
    let target = blocks.iter().find(|b| b.ip == "203.0.113.0/24").expect("block");
    let (status, _) = put(
        &router,
        &format!("/api/v1/admin/ip_blocks/{}", target.id),
        &fx.admin,
        "severity=sign_up_block",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, _) = send(&probe, request_with_peer("/probe", "203.0.113.7:1234", None)).await;
    assert_eq!(status, StatusCode::OK);
    // ...but it flags registration.
    assert!(admin_infra::is_sign_up_blocked_for(
        &probe_state,
        "203.0.113.7".parse().unwrap()
    ));
}

/// A probe route behind the enforcement middleware.
fn probe_parts(srv: &TestServer, trusted_proxies: Vec<IpAddr>) -> (axum::Router, ApiState) {
    use axum::routing::get;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let cfg = Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(srv.streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(trusted_proxies),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: srv.streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(
            birchtree::application::accounts::MemoryInviteStore::default(),
        ),
    };
    let router = axum::Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_infra::ip_block_middleware,
        ))
        .with_state(state.clone());
    (router, state)
}

fn request_with_peer(uri: &str, peer: &str, forwarded_for: Option<&str>) -> Request<Body> {
    let addr: SocketAddr = peer.parse().expect("peer");
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(xff) = forwarded_for {
        builder = builder.header("x-forwarded-for", xff);
    }
    let mut request = builder.body(Body::empty()).expect("request");
    request.extensions_mut().insert(ConnectInfo(addr));
    request
}

// ---------------------------------------------------------------------------
// Canonical email blocks.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_canonical_email_blocks_crud_and_test() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    let (status, body) = get(&router, "/api/v1/admin/canonical_email_blocks", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    // Create by email: canonicalized (dots/+tag stripped, downcased) + hashed.
    let (status, body) = post(
        &router,
        "/api/v1/admin/canonical_email_blocks",
        &fx.admin,
        "email=Foo.Bar%2Btag%40Example.COM",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = sha256_hex("foobar@example.com".as_bytes());
    assert_eq!(body["canonical_email_hash"], expected.as_str());
    let id = body["id"].as_str().expect("id").to_string();

    // `test` takes the READ scope upstream (kept): the read-only token works,
    // a write-only token is refused with the scope error.
    let (status, body) = post(
        &router,
        "/api/v1/admin/canonical_email_blocks/test",
        &fx.admin_read,
        "email=foo.bar%40example.com",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 1);
    assert_eq!(body[0]["canonical_email_hash"], expected.as_str());

    let (status, body) = post(
        &router,
        "/api/v1/admin/canonical_email_blocks/test",
        &fx.admin_write_only,
        "email=foo.bar%40example.com",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is outside the authorized scopes");

    // Missing email → 400 (upstream `params.require`).
    let (status, _) = post(&router, "/api/v1/admin/canonical_email_blocks/test", &fx.admin_read, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A non-matching email tests clean.
    let (status, body) = post(
        &router,
        "/api/v1/admin/canonical_email_blocks/test",
        &fx.admin_read,
        "email=someone%40else.example",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    // Invalid create inputs.
    let (status, _) = post(&router, "/api/v1/admin/canonical_email_blocks", &fx.admin, "email=not-an-email").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&router, "/api/v1/admin/canonical_email_blocks", &fx.admin, "").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = get(&router, &format!("/api/v1/admin/canonical_email_blocks/{id}"), Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["canonical_email_hash"], expected.as_str());
    let (status, _) = get(&router, "/api/v1/admin/canonical_email_blocks/777001", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = delete(&router, &format!("/api/v1/admin/canonical_email_blocks/{id}"), &fx.admin).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Tags.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_tags_materialize_and_update() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    // No hashtags yet: the registry is empty (never fabricated).
    let (status, body) = get(&router, "/api/v1/admin/tags", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    // Post a status with a hashtag through the public router.
    let poster = mint_token(&srv, fx.app_id, Some(admin_id_of(&srv, "admin")), "write:statuses", "poster");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=hello+%23TagOne", Some(poster.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // The registry materializes the real tag.
    let (status, body) = get(&router, "/api/v1/admin/tags", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tags = body.as_array().expect("array");
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0]["name"], "tagone");
    assert_eq!(tags[0]["display_name"], "tagone");
    assert_eq!(tags[0]["requires_review"], true);
    assert!(tags[0]["url"].as_str().expect("url").ends_with("/tags/tagone"));
    let id = tags[0]["id"].as_str().expect("id").to_string();

    let (status, body) = get(&router, &format!("/api/v1/admin/tags/{id}"), Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "tagone");
    let (status, _) = get(&router, "/api/v1/admin/tags/424242", Some(fx.admin.as_str())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Update flags (PUT + PATCH alias); `reviewed_at` stamps on write.
    let (status, body) = put(
        &router,
        &format!("/api/v1/admin/tags/{id}"),
        &fx.admin,
        "display_name=TagOne&trendable=true&usable=false&listable=true",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["display_name"], "TagOne");
    assert_eq!(body["trendable"], true);
    assert_eq!(body["usable"], false);
    assert_eq!(body["listable"], true);
    assert_eq!(body["requires_review"], false);
    let (status, body) = patch(&router, &format!("/api/v1/admin/tags/{id}"), &fx.admin, "usable=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["usable"], true);

    // Blank display_name → 422.
    let (status, _) = put(&router, &format!("/api/v1/admin/tags/{id}"), &fx.admin, "display_name=").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------------
// Measures / dimensions / retention (all POST, all read-scoped upstream).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_measures_read_scope_and_real_keys() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    // Unknown keys are omitted (upstream `filter_map` semantics).
    let (status, body) = post(&router, "/api/v1/admin/measures", &fx.admin_read, "keys%5B%5D=bogus").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));

    // A write-only admin token is refused: these POSTs take the read scope.
    let (status, body) = post(&router, "/api/v1/admin/measures", &fx.admin_write_only, "keys%5B%5D=new_users").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is outside the authorized scopes");

    // Real keys over the fixture window (accounts were created just now).
    let now = now_ms();
    let body_str = format!("keys%5B%5D=new_users&keys%5B%5D=opened_reports&keys%5B%5D=instance_accounts&keys%5B%5D=instance_statuses&keys%5B%5D=instance_reports&keys%5B%5D=instance_media_attachments&start_at={}&end_at={}", now - 86_400_000, now + 60_000);
    let (status, body) = post(&router, "/api/v1/admin/measures", &fx.admin_read, &body_str).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 6);

    let by_key = |key: &str| -> &Value {
        rows.iter().find(|row| row["key"] == key).unwrap_or_else(|| panic!("missing {key}: {body}"))
    };
    // 5 fixture accounts (admin/reader/writer/mod/user) created in-window.
    let new_users = by_key("new_users");
    assert_eq!(new_users["total"], "5");
    assert_eq!(new_users["previous_total"], "0");
    assert!(new_users["data"].is_array());
    // No reports or statuses yet: honest zeroes, instance keys carry no
    // `previous_total` (upstream `total_in_time_range? == false`).
    assert_eq!(by_key("opened_reports")["total"], "0");
    assert_eq!(by_key("instance_accounts")["total"], "5");
    assert!(by_key("instance_accounts").get("previous_total").is_none());
    assert_eq!(by_key("instance_statuses")["total"], "0");
    assert!(by_key("instance_statuses").get("previous_total").is_none());
    assert_eq!(by_key("instance_reports")["total"], "0");
    let media = by_key("instance_media_attachments");
    assert_eq!(media["unit"], "bytes");
    assert_eq!(media["total"], "0");
    assert_eq!(media["human_value"], "0 Bytes");

    // Uncomputable known keys are omitted, never fabricated. D39 serves
    // `active_users` (sign-in telemetry) and `resolved_reports`
    // (`action_taken_at`), so they render honest zeroes here;
    // `interactions` (no event log) and `tag_uses` (no tag history) stay
    // omitted.
    let (status, body) = post(
        &router,
        "/api/v1/admin/measures",
        &fx.admin_read,
        "keys%5B%5D=active_users&keys%5B%5D=interactions&keys%5B%5D=resolved_reports&keys%5B%5D=tag_uses",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("array");
    let keys: Vec<&str> = rows.iter().map(|row| row["key"].as_str().expect("key")).collect();
    assert!(keys.contains(&"active_users"), "{body}");
    assert!(keys.contains(&"resolved_reports"), "{body}");
    assert!(keys.contains(&"interactions"), "{body}");
    assert!(!keys.contains(&"tag_uses"), "{body}");
}

#[tokio::test]
async fn admin_dimensions_real_keys() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    // One status with a language + app name for the language/source counts.
    let poster = mint_token(&srv, fx.app_id, Some(admin_id_of(&srv, "admin")), "write:statuses", "dimposter");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=hello&language=en", Some(poster.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let now = now_ms();
    let body_str = format!(
        "keys%5B%5D=languages&keys%5B%5D=sources&keys%5B%5D=servers&keys%5B%5D=space_usage&start_at={}&end_at={}",
        now - 86_400_000,
        now + 60_000
    );
    let (status, body) = post(&router, "/api/v1/admin/dimensions", &fx.admin_read, &body_str).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 4);
    let by_key = |key: &str| -> &Value {
        rows.iter().find(|row| row["key"] == key).unwrap_or_else(|| panic!("missing {key}: {body}"))
    };
    let languages = by_key("languages");
    assert!(languages["data"].as_array().expect("data").iter().any(|entry| entry["key"] == "en" && entry["value"] == "1"));
    let sources = by_key("sources");
    assert!(sources["data"].as_array().expect("data").iter().any(|entry| entry["value"] == "1"));
    let servers = by_key("servers");
    assert!(servers["data"].as_array().expect("data").iter().any(|entry| entry["key"] == "example.social" && entry["value"] == "1"));
    let space = by_key("space_usage");
    let media = space["data"].as_array().expect("data").iter().find(|entry| entry["key"] == "media").expect("media");
    assert_eq!(media["unit"], "bytes");
    assert!(media["human_value"].is_string());

    // Uncomputable dimensions are omitted.
    let (status, body) = post(&router, "/api/v1/admin/dimensions", &fx.admin_read, "keys%5B%5D=software_versions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(Vec::new()));
}

#[tokio::test]
async fn admin_retention_requires_window_and_counts_posters() {
    let srv = TestServer::new(false);
    let router = admin_router(&srv, Vec::new());
    let fx = fixtures(&srv);

    // Missing bounds → 400 (upstream `params.require`).
    let (status, _) = post(&router, "/api/v1/admin/retention", &fx.admin_read, "frequency=day").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // One poster with a status today; the rest of the cohort is silent.
    let poster = mint_token(&srv, fx.app_id, Some(admin_id_of(&srv, "admin")), "write:statuses", "retposter");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=retention+probe", Some(poster.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let now = now_ms();
    let body_str = format!("start_at={}&end_at={}&frequency=day", now - 86_400_000, now + 60_000);
    let (status, body) = post(&router, "/api/v1/admin/retention", &fx.admin_read, &body_str).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cohorts = body.as_array().expect("array");
    assert!(!cohorts.is_empty());
    assert_eq!(cohorts[0]["frequency"], "day");
    let data = cohorts[0]["data"].as_array().expect("data");
    assert!(!data.is_empty());
    // Exactly one of the five cohort members posted in the window.
    let total_active: u64 = data
        .iter()
        .map(|point| point["value"].as_str().expect("value").parse::<u64>().expect("count"))
        .sum();
    assert_eq!(total_active, 1);
    // The rate divides by the cohort size (5 fixture accounts).
    let rate: f64 = data[0]["rate"].as_f64().expect("rate");
    assert!((rate - 1.0 / 5.0).abs() < 1e-9, "{rate}");
}
