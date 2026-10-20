//! P1 S2 announcements contract tests (subagent S2).
//!
//! Pin (`8ff11c2b`) behavior under test:
//! - `GET /api/v1/announcements` serves published announcements with the
//!   `REST::AnnouncementSerializer` fields (`announcements_controller.rb:8-10`
//!   + `announcement_serializer.rb`).
//! - `POST .../dismiss` answers `200 {}` (not 204) and marks read
//!   (`announcements_controller.rb:12-15`; `render_empty` = `200 {}`, per
//!   `api/base_controller.rb:86-88`).
//! - `PUT`/`DELETE .../reactions/:id` answer `200 {}`; 404 when the
//!   announcement is unpublished or the reaction is missing; 422 on invalid
//!   emoji (`reactions_controller.rb` + `reaction_validator.rb`). No feature
//!   flag gates either action at the pin.
//! - Admin authoring is new API surface (upstream is HTML-only); admins-only
//!   (`manage_announcements` is administration-only per `config/roles.yml`).
//!
//! The admin routes are mounted on a test-local router (production
//! `router.rs` is parent-owned; the mount snippet lives in
//! `application/announcements.rs`). Both routers share the server's store,
//! hence the same announcement rows.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use birchtree::api::mastodon::endpoints::announcements;
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::context::Context;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::ports::{AccountStore, EmojiStore, NewApp, NewToken, OAuthStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, CustomEmoji, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};

use common::{TEST_PUBLIC_URL, TestServer, form_request, get_request, json, send};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const USER_SCOPES: &str = "read write write:accounts write:favourites";
const ADMIN_SCOPES: &str = "admin:read admin:write";

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

fn register_app(srv: &TestServer) -> Id {
    srv.store
        .register_app(NewApp {
            name: "ann-tests".to_string(),
            client_id: "ann-client".to_string(),
            client_secret_hash: None,
            redirect_uris: Vec::new(),
            scopes: vec!["read".to_string(), "write".to_string()],
            website: None,
            now_ms: now_ms(),
        })
        .expect("register app")
        .id
}

fn mint_token(srv: &TestServer, app_id: Id, user: Option<Id>, scopes: &str, tag: &str) -> String {
    let plaintext = format!("ann-{tag}-{}", scopes.replace([' ', ':'], "-"));
    srv.store
        .issue_access_token(NewToken {
            app_id,
            user_id: user,
            token_hash: sha256_hex(plaintext.as_bytes()),
            scopes: scopes.split_whitespace().map(str::to_string).collect(),
            now_ms: now_ms(),
            expires_at_ms: None,
        })
        .expect("issue token");
    plaintext
}

fn mk_user(srv: &TestServer, username: &str) -> Id {
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
}

/// Test-local admin router sharing the server's store (mount snippet for the
/// parent's `router.rs` lives in `application/announcements.rs`).
fn admin_router(srv: &TestServer) -> axum::Router {
    use axum::routing::{get, post};
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
        trusted_proxies: Arc::new(Vec::new()),
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
    axum::Router::new()
        .route(
            "/api/v1/admin/announcements",
            get(announcements::admin_index).post(announcements::admin_create),
        )
        .route(
            "/api/v1/admin/announcements/:id",
            get(announcements::admin_show)
                .put(announcements::admin_update)
                .patch(announcements::admin_update)
                .delete(announcements::admin_destroy),
        )
        .route(
            "/api/v1/admin/announcements/:id/publish",
            post(announcements::admin_publish),
        )
        .route(
            "/api/v1/admin/announcements/:id/unpublish",
            post(announcements::admin_unpublish),
        )
        .with_state(state)
}

fn json_request(method: &str, uri: &str, body: Value, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("request")
}

fn empty_post(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

struct Fixtures {
    app_id: Id,
    admin: String,
    moderator: String,
    user: String,
    app_only: String,
}

fn fixtures(srv: &TestServer) -> Fixtures {
    let app_id = register_app(srv);
    let admin_id = mk_user(srv, "ann_admin");
    srv.store
        .set_user_role(admin_id, UserRole::Admin)
        .expect("role");
    let mod_id = mk_user(srv, "ann_mod");
    srv.store
        .set_user_role(mod_id, UserRole::Moderator)
        .expect("role");
    let user_id = mk_user(srv, "ann_bob");
    mk_user(srv, "ann_alice");
    Fixtures {
        admin: mint_token(srv, app_id, Some(admin_id), ADMIN_SCOPES, "admin"),
        moderator: mint_token(srv, app_id, Some(mod_id), ADMIN_SCOPES, "mod"),
        user: mint_token(srv, app_id, Some(user_id), USER_SCOPES, "user"),
        app_only: mint_token(srv, app_id, None, USER_SCOPES, "app"),
        app_id,
    }
}

async fn admin_create(admin: &axum::Router, token: &str, body: Value) -> Value {
    let (status, _h, bytes) = send(
        admin,
        json_request("POST", "/api/v1/admin/announcements", body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin create: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn announcements_list_shape() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);

    let created = admin_create(
        &admin,
        &fx.admin,
        json!({"text": "Hello @ann_alice #news", "all_day": true}),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_string();

    let (status, _h, bytes) = send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    assert_eq!(status, StatusCode::OK, "index: {}", String::from_utf8_lossy(&bytes));
    let list = json(&bytes);
    let item = list
        .as_array()
        .expect("array")
        .iter()
        .find(|v| v["id"] == id)
        .expect("created announcement listed")
        .clone();

    // Serializer fields at the pin (announcement_serializer.rb): id, content,
    // starts_at, ends_at, all_day, published_at, updated_at, read, mentions,
    // statuses, tags, emojis, reactions. Note: NO `published` field upstream.
    assert!(item["id"].is_string(), "id is a decimal string");
    assert_eq!(item["content"], json!("Hello @ann_alice #news"));
    assert_eq!(item["starts_at"], Value::Null);
    assert_eq!(item["ends_at"], Value::Null);
    assert_eq!(item["all_day"], json!(true));
    assert!(item["published_at"].is_string(), "published_at set: {item}");
    assert!(item["updated_at"].is_string(), "updated_at set: {item}");
    assert_eq!(item["read"], json!(false));
    assert_eq!(item["statuses"], json!([]));
    assert_eq!(item["emojis"], json!([]));
    assert_eq!(item["reactions"], json!([]));
    let mentions = item["mentions"].as_array().expect("mentions");
    assert_eq!(mentions.len(), 1, "alice mention resolves: {item}");
    assert!(mentions[0]["id"].is_string());
    assert_eq!(mentions[0]["username"], json!("ann_alice"));
    assert!(mentions[0]["url"].is_string());
    assert_eq!(mentions[0]["acct"], json!("ann_alice"));
    let tags = item["tags"].as_array().expect("tags");
    assert_eq!(tags.len(), 1, "hashtag extracts: {item}");
    assert_eq!(tags[0]["name"], json!("news"));
    assert!(tags[0]["url"].as_str().unwrap().ends_with("tags/news"), "tag url: {}", tags[0]);

    // Unauthenticated reads are refused (upstream `require_user!`).
    let (status, _h, _b) = send(&srv.router, get_request("/api/v1/announcements", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn announcements_dismiss_returns_200_empty_and_marks_read() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);
    let created = admin_create(&admin, &fx.admin, json!({"text": "dismiss me"})).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Upstream `render_empty` is `200 {}` — NOT 204.
    let (status, _h, bytes) = send(
        &srv.router,
        empty_post(&format!("/api/v1/announcements/{id}/dismiss"), Some(&fx.user)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dismiss: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), json!({}));

    // The row now reads `read: true` for the dismisser.
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    assert_eq!(status, StatusCode::OK);
    let item = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(item["read"], json!(true));

    // Unknown ids (and unpublished rows) are 404.
    let (status, _h, _b) = send(
        &srv.router,
        empty_post("/api/v1/announcements/1/dismiss", Some(&fx.user)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // App-only tokens get the user-required 422.
    let (status, _h, _b) = send(
        &srv.router,
        empty_post(&format!("/api/v1/announcements/{id}/dismiss"), Some(&fx.app_only)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn announcements_reaction_round_trip() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);
    let created = admin_create(&admin, &fx.admin, json!({"text": "react here"})).await;
    let id = created["id"].as_str().unwrap().to_string();
    let put = |name: &str| {
        Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/announcements/{id}/reactions/{name}"))
            .header("authorization", format!("Bearer {}", fx.user))
            .body(Body::empty())
            .unwrap()
    };
    let reaction_path = |name: &str| format!("/api/v1/announcements/{id}/reactions/{name}");

    // PUT answers `200 {}` (upstream `render_empty`).
    let encoded = "%F0%9F%8E%89"; // 🎉
    let (status, _h, bytes) = send(&srv.router, put(encoded)).await;
    assert_eq!(status, StatusCode::OK, "put: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), json!({}));

    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    let item = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()
        .clone();
    let reactions = item["reactions"].as_array().unwrap().clone();
    assert_eq!(reactions.len(), 1, "one grouped reaction: {reactions:?}");
    assert_eq!(reactions[0]["name"], json!("🎉"));
    assert_eq!(reactions[0]["count"], json!(1));
    assert_eq!(reactions[0]["me"], json!(true));
    assert_eq!(reactions[0]["url"], Value::Null);

    // Repeated PUT is idempotent (single row, count stays 1).
    let (status, _h, _b) = send(&srv.router, put(encoded)).await;
    assert_eq!(status, StatusCode::OK);
    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    let again = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()["reactions"]
        .clone();
    assert_eq!(again.as_array().unwrap().len(), 1);
    assert_eq!(again[0]["count"], json!(1));

    // DELETE answers `200 {}` and clears the reaction; a second DELETE is 404.
    let del = || {
        Request::builder()
            .method("DELETE")
            .uri(reaction_path(encoded))
            .header("authorization", format!("Bearer {}", fx.user))
            .body(Body::empty())
            .unwrap()
    };
    let (status, _h, bytes) = send(&srv.router, del()).await;
    assert_eq!(status, StatusCode::OK, "delete: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), json!({}));
    let (status, _h, _b) = send(&srv.router, del()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Unknown announcements are 404 on both verbs.
    let (status, _h, _b) = send(
        &srv.router,
        Request::builder()
            .method("PUT")
            .uri("/api/v1/announcements/1/reactions/%F0%9F%8E%89")
            .header("authorization", format!("Bearer {}", fx.user))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Shortcode-shaped names without a matching custom emoji are 422
    // (ReactionValidator: unrecognized emoji).
    let (status, _h, bytes) = send(
        &srv.router,
        Request::builder()
            .method("PUT")
            .uri(reaction_path("nope_missing"))
            .header("authorization", format!("Bearer {}", fx.user))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "422: {}", String::from_utf8_lossy(&bytes));
}

#[tokio::test]
async fn announcements_custom_emoji_reaction_carries_urls() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);
    srv.store
        .upsert_emoji(CustomEmoji {
            shortcode: "party_parrot".to_string(),
            image_key: "custom_emojis/images/xx/original/party_parrot.png".to_string(),
            static_image_key: "custom_emojis/images/xx/static/party_parrot.png".to_string(),
            visible_in_picker: true,
            disabled: false,
            category: None,
        })
        .expect("upsert emoji");
    let created = admin_create(
        &admin,
        &fx.admin,
        json!({"text": "dance :party_parrot:"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // `:shortcode:` in text surfaces in `emojis`.
    assert_eq!(created["emojis"].as_array().unwrap().len(), 1);
    assert_eq!(created["emojis"][0]["shortcode"], json!("party_parrot"));

    let (status, _h, _b) = send(
        &srv.router,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/announcements/{id}/reactions/party_parrot"))
            .header("authorization", format!("Bearer {}", fx.user))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    let item = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == id)
        .unwrap()
        .clone();
    let reactions = item["reactions"].as_array().unwrap().clone();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0]["name"], json!("party_parrot"));
    assert_eq!(reactions[0]["me"], json!(true));
    assert!(reactions[0]["url"].as_str().unwrap().contains("party_parrot"));
    assert!(reactions[0]["static_url"].as_str().unwrap().contains("party_parrot"));
}

#[tokio::test]
async fn announcements_admin_gate_refuses_non_admins() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);
    let body = json!({"text": "gated"});

    // 401 without a token.
    let (status, _h, _b) = send(
        &admin,
        json_request("POST", "/api/v1/admin/announcements", body.clone(), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 403 for role-less and moderator callers (role before scope; upstream
    // `manage_announcements` is administration-only).
    for token in [&fx.user, &fx.moderator] {
        let (status, _h, bytes) = send(
            &admin,
            json_request("POST", "/api/v1/admin/announcements", body.clone(), Some(token)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "gate: {}", String::from_utf8_lossy(&bytes));
    }

    // Admin without the write scope is 403 (scope after role).
    let narrow = mint_token(&srv, fx.app_id, Some(admin_id(&srv)), "admin:read", "narrow");
    let (status, _h, _b) = send(
        &admin,
        json_request("POST", "/api/v1/admin/announcements", body.clone(), Some(&narrow)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Validation: blank text is 422.
    let (status, _h, _b) = send(
        &admin,
        json_request("POST", "/api/v1/admin/announcements", json!({"text": " "}), Some(&fx.admin)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

fn admin_id(srv: &TestServer) -> Id {
    srv.store
        .account_by_handle("ann_admin", "example.social")
        .expect("lookup")
        .expect("admin present")
        .id
}

#[tokio::test]
async fn announcements_admin_lifecycle() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let admin = admin_router(&srv);

    // Future-scheduled rows stay unpublished (upstream `set_published`) and
    // are invisible to the public index.
    let future = "2999-01-01T00:00:00Z";
    let created = admin_create(
        &admin,
        &fx.admin,
        json!({"text": "later", "scheduled_at": future}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["published_at"], Value::Null);
    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    assert!(
        json(&bytes).as_array().unwrap().iter().all(|v| v["id"] != id),
        "draft hidden from public index"
    );

    // Publish flips it visible; unpublish hides it again.
    let (status, _h, bytes) = send(
        &admin,
        empty_post(&format!("/api/v1/admin/announcements/{id}/publish"), Some(&fx.admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish: {}", String::from_utf8_lossy(&bytes));
    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    assert!(json(&bytes).as_array().unwrap().iter().any(|v| v["id"] == id));

    let (status, _h, _b) = send(
        &admin,
        empty_post(&format!("/api/v1/admin/announcements/{id}/unpublish"), Some(&fx.admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_s, _h, bytes) =
        send(&srv.router, get_request("/api/v1/announcements", Some(&fx.user), None)).await;
    assert!(json(&bytes).as_array().unwrap().iter().all(|v| v["id"] != id));

    // Update edits text; destroy removes the row (`200 {}`).
    let (status, _h, bytes) = send(
        &admin,
        json_request(
            "PATCH",
            &format!("/api/v1/admin/announcements/{id}"),
            json!({"text": "revised"}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["content"], json!("revised"));

    let (status, _h, bytes) = send(
        &admin,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/admin/announcements/{id}"))
            .header("authorization", format!("Bearer {}", fx.admin))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "destroy: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), json!({}));

    // Half windows are 422 either way (upstream presence validations).
    for body in [json!({"text": "x", "starts_at": "2999-01-01T00:00:00Z"}),
                 json!({"text": "x", "ends_at": "2999-01-01T00:00:00Z"})] {
        let (status, _h, _b) = send(
            &admin,
            json_request("POST", "/api/v1/admin/announcements", body, Some(&fx.admin)),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // The destroyed row dismisses 404 (published scope).
    let (status, _h, _b) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/announcements/{id}/dismiss"), "", Some(&fx.user)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
