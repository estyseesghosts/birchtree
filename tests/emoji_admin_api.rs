//! Admin custom-emoji API tests (subagent W1).
//!
//! Pin (`8ff11c2b`) findings: no upstream REST admin custom-emoji API
//! exists (`config/routes/api.rb:78` exposes only `custom_emojis#index`;
//! authoring upstream is the HTML `Admin::CustomEmojisController`), and
//! `manage_custom_emojis` is administration-only (`config/roles.yml`), so
//! moderators are refused. The routes under test are new API surface on a
//! test-local router (production `router.rs` is parent-owned; the mount
//! snippet is proposed alongside this change).

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use birchtree::api::mastodon::endpoints::{admin_emoji, emoji as emoji_ep};
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::context::Context;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::emoji as emoji_uc;
use birchtree::application::ports::{AccountStore, EmojiStore, NewApp, NewToken, OAuthStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};

use common::{TEST_PUBLIC_URL, TestServer, get_request, json, send};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const USER_SCOPES: &str = "read write write:accounts";
const ADMIN_SCOPES: &str = "admin:read admin:write";

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

fn register_app(srv: &TestServer) -> Id {
    srv.store
        .register_app(NewApp {
            name: "emoji-admin-tests".to_string(),
            client_id: "emoji-admin-client".to_string(),
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
    let plaintext = format!("emoji-{tag}-{}", scopes.replace([' ', ':'], "-"));
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

/// Test-local router sharing the server's store (mount snippet for the
/// parent's `router.rs` is proposed alongside this change).
fn admin_router(srv: &TestServer) -> axum::Router {
    use axum::routing::{get, put};
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
            "/api/v1/admin/custom_emojis",
            get(admin_emoji::index).post(admin_emoji::create),
        )
        .route(
            "/api/v1/admin/custom_emojis/:shortcode",
            put(admin_emoji::update).delete(admin_emoji::destroy),
        )
        .route(
            "/system/custom_emojis/images/*path",
            get(emoji_ep::serve_image),
        )
        .route("/api/v1/custom_emojis", get(emoji_ep::index))
        .with_state(state)
}

struct Fixtures {
    admin: String,
    admin_noscope: String,
    moderator: String,
    user: String,
    app_only: String,
}

fn fixtures(srv: &TestServer) -> Fixtures {
    let app_id = register_app(srv);
    let admin_id = mk_user(srv, "emoji_admin");
    srv.store
        .set_user_role(admin_id, UserRole::Admin)
        .expect("role");
    let mod_id = mk_user(srv, "emoji_mod");
    srv.store
        .set_user_role(mod_id, UserRole::Moderator)
        .expect("role");
    let user_id = mk_user(srv, "emoji_bob");
    Fixtures {
        admin: mint_token(srv, app_id, Some(admin_id), ADMIN_SCOPES, "admin"),
        admin_noscope: mint_token(srv, app_id, Some(admin_id), USER_SCOPES, "admin-noscope"),
        moderator: mint_token(srv, app_id, Some(mod_id), ADMIN_SCOPES, "mod"),
        user: mint_token(srv, app_id, Some(user_id), USER_SCOPES, "user"),
        app_only: mint_token(srv, app_id, None, USER_SCOPES, "app"),
    }
}

// ---------------------------------------------------------------------------
// Multipart upload bodies (multipart precedent from `endpoints/media.rs`).
// ---------------------------------------------------------------------------

const BOUNDARY: &str = "emoji-test-boundary";

/// A real 1x1 PNG whose single pixel is derived from `tag` (same encoder
/// stack the server validates with — never synthetic magic bytes: the
/// parser's `IHDR` checks stay strict).
fn png(tag: u8) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([tag.wrapping_mul(37).wrapping_add(11), 20, 30, 255]),
    ));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("png");
    cursor.into_inner()
}

fn multipart_request(
    uri: &str,
    token: Option<&str>,
    shortcode: Option<&str>,
    image: Option<&[u8]>,
    category: Option<&str>,
    visible: Option<&str>,
) -> Request<Body> {
    let mut body = Vec::new();
    let mut part = |name: &str, filename: Option<&str>, bytes: &[u8]| {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match filename {
            Some(file) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    };
    if let Some(code) = shortcode {
        part("shortcode", None, code.as_bytes());
    }
    if let Some(bytes) = image {
        part("image", Some("emoji.png"), bytes);
    }
    if let Some(cat) = category {
        part("category", None, cat.as_bytes());
    }
    if let Some(vis) = visible {
        part("visible_in_picker", None, vis.as_bytes());
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let mut builder = Request::builder().method("POST").uri(uri).header(
        "content-type",
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body)).expect("request")
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

fn image_path(row: &Value) -> String {
    let url = row["url"].as_str().expect("url");
    let path = url.strip_prefix(TEST_PUBLIC_URL).expect("asset path");
    assert!(path.starts_with("/system/custom_emojis/images/"), "{url}");
    path.to_string()
}

async fn admin_list(router: &axum::Router, token: &str) -> Value {
    let (status, _, bytes) = send(
        router,
        get_request("/api/v1/admin/custom_emojis", Some(token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin list: {}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)
}

fn delete_request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_emoji_auth_matrix() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let router = admin_router(&srv);

    // Anonymous → 401.
    let (status, _, _) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Plain user → 403 Forbidden (role before scope).
    let (status, _, bytes) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", Some(&fx.user), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");

    // Moderator with admin scopes → 403 (manage_custom_emojis is admin-only).
    let (status, _, bytes) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", Some(&fx.moderator), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");

    // App-only token → 422 (user required).
    let (status, _, _) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", Some(&fx.app_only), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Admin without admin scopes → 403 scopes message.
    let (status, _, bytes) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", Some(&fx.admin_noscope), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        json(&bytes)["error"],
        "This action is outside the authorized scopes"
    );

    // Admin with scope → 200 (empty list).
    let (status, _, bytes) = send(
        &router,
        get_request("/api/v1/admin/custom_emojis", Some(&fx.admin), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin: {}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(json(&bytes), json!([]));

    // Write paths are gated too (plain user + moderator POST → 403).
    for token in [&fx.user, &fx.moderator] {
        let (status, _, bytes) = send(
            &router,
            multipart_request(
                "/api/v1/admin/custom_emojis",
                Some(token),
                Some("nope"),
                Some(&png(1)),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "write gate");
        assert_eq!(json(&bytes)["error"], "Forbidden");
    }
}

#[tokio::test]
async fn admin_emoji_create_validation() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let router = admin_router(&srv);

    // Bad shortcodes → 422.
    for bad in ["x", "bad code!", "a"] {
        let (status, _, bytes) = send(
            &router,
            multipart_request(
                "/api/v1/admin/custom_emojis",
                Some(&fx.admin),
                Some(bad),
                Some(&png(1)),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "shortcode {bad}");
        assert!(json(&bytes).get("error").is_some());
    }

    // Non-image bytes → 422 (magic-byte check).
    let (status, _, bytes) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("notanimage"),
            Some(b"plain text not an upload at all"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "File content type is invalid, File is invalid"
    );

    // Oversize image (>256 KB, valid PNG magic) → 422.
    let mut big = png(9);
    big.resize(300 * 1024, 0x42);
    let (status, _, bytes) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("toobig"),
            Some(&big),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "Image must be less than 256 KB");

    // Non-multipart POST → 422 (blank shortcode).
    let (status, _, _) = send(
        &router,
        json_request(
            "POST",
            "/api/v1/admin/custom_emojis",
            json!({"shortcode": "jsonbody"}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Invalid visible_in_picker value → 422.
    let (status, _, _) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("badvis"),
            Some(&png(1)),
            None,
            Some("maybe"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn admin_emoji_crud_replace_delete() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let router = admin_router(&srv);

    // Create.
    let (status, _, bytes) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("dance"),
            Some(&png(1)),
            Some("fun"),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create: {}",
        String::from_utf8_lossy(&bytes)
    );
    let row = json(&bytes);
    assert_eq!(row["shortcode"], "dance");
    assert_eq!(row["visible_in_picker"], true);
    assert_eq!(row["category"], "fun");
    assert_eq!(row["disabled"], false);
    let path = image_path(&row);

    // Image bytes are served.
    let (status, _, served) = send(&router, get_request(&path, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(served, png(1));

    // Public index lists it.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/custom_emojis", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json(&bytes)
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["shortcode"] == "dance")
    );

    // Disable + hide via PUT.
    let (status, _, bytes) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/dance",
            json!({"disabled": true, "visible_in_picker": false}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "update: {}",
        String::from_utf8_lossy(&bytes)
    );
    let row = json(&bytes);
    assert_eq!(row["disabled"], true);
    assert_eq!(row["visible_in_picker"], false);

    // Public index drops it; admin list keeps it with full fields.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/custom_emojis", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !json(&bytes)
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["shortcode"] == "dance")
    );
    let list = admin_list(&router, &fx.admin).await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["shortcode"] == "dance")
        .expect("admin row")
        .clone();
    assert_eq!(entry["disabled"], true);
    assert_eq!(entry["category"], "fun");

    // Re-POST replaces bytes and metadata.
    let (status, _, bytes) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("dance"),
            Some(&png(2)),
            Some("games"),
            Some("true"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "replace: {}",
        String::from_utf8_lossy(&bytes)
    );
    let row = json(&bytes);
    assert_eq!(row["category"], "games");
    let (status, _, served) = send(&router, get_request(&image_path(&row), None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(served, png(2));

    // Delete removes the row and its bytes.
    let (status, _, bytes) = send(
        &router,
        delete_request("/api/v1/admin/custom_emojis/dance", &fx.admin),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "delete: {}",
        String::from_utf8_lossy(&bytes)
    );
    let (status, _, _) = send(&router, get_request(&path, None, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let list = admin_list(&router, &fx.admin).await;
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["shortcode"] == "dance")
    );

    // Second delete + update of a missing row → 404.
    let (status, _, _) = send(
        &router,
        delete_request("/api/v1/admin/custom_emojis/dance", &fx.admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/dance",
            json!({"disabled": true}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_emoji_category_update() {
    let srv = TestServer::new(false);
    let fx = fixtures(&srv);
    let router = admin_router(&srv);

    let (status, _, bytes) = send(
        &router,
        multipart_request(
            "/api/v1/admin/custom_emojis",
            Some(&fx.admin),
            Some("catmoji"),
            Some(&png(3)),
            Some("aaa"),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create: {}",
        String::from_utf8_lossy(&bytes)
    );

    // Null clears the category.
    let (status, _, bytes) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/catmoji",
            json!({"category": null}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["category"], Value::Null);

    // New value sets it; empty object and wrong types → 422.
    let (status, _, bytes) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/catmoji",
            json!({"category": "bbb"}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["category"], "bbb");

    let (status, _, _) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/catmoji",
            json!({}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _, _) = send(
        &router,
        json_request(
            "PUT",
            "/api/v1/admin/custom_emojis/catmoji",
            json!({"disabled": "yes"}),
            Some(&fx.admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The `set_emoji_category` use case directly: set, then missing → NotFound.
    let row =
        emoji_uc::set_emoji_category(&*srv.store, "catmoji", Some("ccc")).expect("set category");
    assert_eq!(row.category.as_deref(), Some("ccc"));
    let row = emoji_uc::set_emoji_category(&*srv.store, "catmoji", None).expect("clear category");
    assert_eq!(row.category, None);
    let err =
        emoji_uc::set_emoji_category(&*srv.store, "nosuchemoji", Some("x")).expect_err("missing");
    assert!(
        matches!(err, birchtree::domain::Error::NotFound(_)),
        "unexpected: {err:?}"
    );
    // Direct store read confirms the cleared row persisted.
    let stored = srv
        .store
        .emoji_by_shortcode("catmoji")
        .expect("read")
        .expect("row");
    assert_eq!(stored.category, None);
}
