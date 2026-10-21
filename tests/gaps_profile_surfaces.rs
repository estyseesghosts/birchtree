//! Gaps batch G-A: operator + profile surfaces.
//!
//! - Password-reset redemption: server-rendered `GET /auth/password/edit`
//!   form + `POST /auth/password` set-password (6h bearers by default,
//!   consume-on-write, indistinguishable 422s, login works afterwards).
//! - Site texts: instance sub-resources serve `--site-*-file` content at
//!   serve time; unconfigured keeps the honest-absence shapes.
//! - Rule translations: `{locale: {text, hint}}` rendering with `{}` fallback.
//! - Locale catalog: real ISO-639 autonyms, never `name` = code.
//! - `GET`/`PATCH /api/v1/profile` parity with the credentials endpoints +
//!   avatar/header `DELETE`; `identity_proofs` is always `[]`.
//!
//! Privacy-sensitive: every account, address and token below is synthetic
//! (`example.*`). No PII, no secrets, no real tokens.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post};

use birchtree::api::mastodon::endpoints::{accounts, instance};
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{CreateAccountCommand, MemoryInviteStore, create_account};
use birchtree::application::context::Context;
use birchtree::application::ports::{AccountStore, AdminInfraStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, EmailToken, EmailTokenKind, IdGenerator, SystemClock, TimestampIdGenerator};

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write profile read:accounts write:accounts";

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

// ---------------------------------------------------------------------------
// Harness: a router mounting the G-A handlers on a custom `ServerInfo`.
// (Production `router.rs` is parent-owned; these mount the same handlers.)
// ---------------------------------------------------------------------------

fn ga_router(srv: &TestServer, cfg: &Config) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let server = Arc::new(ServerInfo::from_config(cfg));
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(srv.streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: srv.streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(MemoryInviteStore::default()),
    };
    axum::Router::new()
        .route("/auth/password/edit", get(accounts::password_reset_page))
        .route("/auth/password", post(accounts::password_reset_update))
        .route(
            "/api/v1/profile",
            get(accounts::profile_show).patch(accounts::profile_update),
        )
        .route("/api/v1/profile/avatar", delete(accounts::profile_avatar_destroy))
        .route("/api/v1/profile/header", delete(accounts::profile_header_destroy))
        .route(
            "/api/v1/accounts/:id/identity_proofs",
            get(accounts::identity_proofs_show),
        )
        .route("/api/v1/instance/rules", get(instance::rules_index))
        .route("/api/v1/instance/languages", get(instance::languages_show))
        .route(
            "/api/v1/instance/extended_description",
            get(instance::extended_description_show),
        )
        .route(
            "/api/v1/instance/privacy_policy",
            get(instance::privacy_policy_show),
        )
        .route(
            "/api/v1/instance/terms_of_service",
            get(instance::terms_of_service_index),
        )
        .route(
            "/api/v1/instance/terms_of_service/:date",
            get(instance::terms_of_service_show),
        )
        .with_state(state)
}

fn plain_config() -> Config {
    Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }
}

async fn get_html(router: &axum::Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let (status, headers, bytes) = send(router, get_request(uri, None, None)).await;
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, content_type, bytes)
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
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// Register an account through the public API; returns the user token.
async fn register_user(srv: &TestServer, username: &str) -> String {
    let app = register_app(srv, ALL_SCOPES).await;
    let token = app_token(srv, &app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// Create a local account with an email address directly (reset-flow fixture).
fn mk_account_with_email(store: &birchtree::persistence::redb::RedbStore, username: &str) -> birchtree::domain::Account {
    create_account(
        store,
        common::TEST_PUBLIC_URL,
        now_ms(),
        CreateAccountCommand {
            username: username.to_string(),
            email: Some(format!("{username}@example.com")),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
}

// ---------------------------------------------------------------------------
// Password reset.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_page_renders_form_with_token() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());
    let (status, content_type, bytes) = get_html(
        &router,
        "/auth/password/edit?reset_password_token=abc123token",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.contains("text/html"), "{content_type}");
    let page = String::from_utf8_lossy(&bytes);
    assert!(page.contains("abc123token"), "token rides the hidden field");
    assert!(page.contains("name=\"password\""), "new-password field");
    assert!(page.contains("name=\"password_confirmation\""), "confirmation field");
    assert!(page.contains("/auth/password"), "posts to the set-password route");
}

#[tokio::test]
async fn reset_round_trip_sets_password_and_burns_token() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());
    let account = mk_account_with_email(&srv.store, "resetvirginia");
    let token = "reset-round-trip-token";
    srv.store
        .store_email_token(
            token,
            EmailToken {
                account_id: account.id,
                kind: EmailTokenKind::PasswordReset,
                created_at_ms: now_ms(),
            },
        )
        .expect("store token");

    let body = format!(
        "reset_password_token={}&password={}&password_confirmation={}",
        enc(token),
        enc("newpassword456"),
        enc("newpassword456"),
    );
    let (status, headers, bytes) =
        send(&router, form_request("POST", "/auth/password", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("text/html"),
        "success is an HTML page"
    );

    // The new password logs in; the old one does not.
    let ok = birchtree::application::auth::authenticate_password(
        &*srv.store,
        "resetvirginia",
        "example.social",
        "newpassword456",
    )
    .expect("auth");
    assert!(ok.is_some(), "new password authenticates");
    let stale = birchtree::application::auth::authenticate_password(
        &*srv.store,
        "resetvirginia",
        "example.social",
        "password123",
    )
    .expect("auth");
    assert!(stale.is_none(), "old password no longer authenticates");

    // Single-use: the same bearer is now indistinguishable from unknown.
    let (retry, _, retry_bytes) =
        send(&router, form_request("POST", "/auth/password", &body, None)).await;
    assert_eq!(retry, StatusCode::UNPROCESSABLE_ENTITY);
    let (unknown, _, unknown_bytes) = send(
        &router,
        form_request(
            "POST",
            "/auth/password",
            "reset_password_token=nope-not-a-token&password=newpassword456&password_confirmation=newpassword456",
            None,
        ),
    )
    .await;
    assert_eq!(unknown, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        retry_bytes, unknown_bytes,
        "used and unknown tokens answer byte-identical 422s"
    );
}

#[tokio::test]
async fn reset_rejects_expired_bearer_by_config_ttl() {
    let srv = TestServer::new(false);
    let account = mk_account_with_email(&srv.store, "resetexpired");

    // Default 6h TTL: a 7h-old bearer is dead, a fresh one is live.
    let router = ga_router(&srv, &plain_config());
    for (token, age_ms) in [("tok-old-7h", 7 * 3_600_000u64), ("tok-fresh", 0u64)] {
        srv.store
            .store_email_token(
                token,
                EmailToken {
                    account_id: account.id,
                    kind: EmailTokenKind::PasswordReset,
                    created_at_ms: now_ms().saturating_sub(age_ms),
                },
            )
            .expect("store token");
    }
    let attempt = |token: &str| {
        format!(
            "reset_password_token={}&password={}&password_confirmation={}",
            enc(token),
            enc("newpassword456"),
            enc("newpassword456"),
        )
    };
    let (status, _, _) =
        send(&router, form_request("POST", "/auth/password", &attempt("tok-old-7h"), None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "7h-old bearer is expired at 6h TTL");

    // A 1h TTL from config: a 2h-old bearer is dead, a 30min-old one is live.
    let mut cfg = plain_config();
    cfg.password_reset_ttl_hours = 1;
    let router = ga_router(&srv, &cfg);
    for (token, age_ms) in [("tok-old-2h", 2 * 3_600_000u64), ("tok-30m", 30 * 60_000u64)] {
        srv.store
            .store_email_token(
                token,
                EmailToken {
                    account_id: account.id,
                    kind: EmailTokenKind::PasswordReset,
                    created_at_ms: now_ms().saturating_sub(age_ms),
                },
            )
            .expect("store token");
    }
    let (status, _, _) =
        send(&router, form_request("POST", "/auth/password", &attempt("tok-old-2h"), None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "2h-old bearer is expired at 1h TTL");
    let (status, _, bytes) =
        send(&router, form_request("POST", "/auth/password", &attempt("tok-30m"), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
}

#[tokio::test]
async fn reset_bad_password_keeps_token_alive() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());
    let account = mk_account_with_email(&srv.store, "resettypo");
    let token = "reset-typo-token";
    srv.store
        .store_email_token(
            token,
            EmailToken {
                account_id: account.id,
                kind: EmailTokenKind::PasswordReset,
                created_at_ms: now_ms(),
            },
        )
        .expect("store token");

    // Too short: 422 form page, token untouched.
    let short = format!(
        "reset_password_token={}&password=short&password_confirmation=short",
        enc(token)
    );
    let (status, _, bytes) =
        send(&router, form_request("POST", "/auth/password", &short, None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        String::from_utf8_lossy(&bytes).contains(token),
        "retry form keeps the token"
    );

    // Mismatch: 422, token untouched.
    let mismatch = format!(
        "reset_password_token={}&password=newpassword456&password_confirmation=otherpassword789",
        enc(token)
    );
    let (status, _, _) =
        send(&router, form_request("POST", "/auth/password", &mismatch, None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The same bearer still redeems afterwards.
    let good = format!(
        "reset_password_token={}&password=newpassword456&password_confirmation=newpassword456",
        enc(token)
    );
    let (status, _, bytes) =
        send(&router, form_request("POST", "/auth/password", &good, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
}

#[tokio::test]
async fn reset_refuses_confirmation_bearer_and_missing_token() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());
    let account = mk_account_with_email(&srv.store, "resetwrongkind");
    srv.store
        .store_email_token(
            "reset-confirmation-kind",
            EmailToken {
                account_id: account.id,
                kind: EmailTokenKind::Confirmation,
                created_at_ms: now_ms(),
            },
        )
        .expect("store token");

    // Wrong-kind bearer: the same indistinguishable 422 (consumed, like
    // `confirm_email`'s wrong-endpoint paste).
    let (status, _, wrong_bytes) = send(
        &router,
        form_request(
            "POST",
            "/auth/password",
            "reset_password_token=reset-confirmation-kind&password=newpassword456&password_confirmation=newpassword456",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (unknown, _, unknown_bytes) = send(
        &router,
        form_request(
            "POST",
            "/auth/password",
            "reset_password_token=never-existed&password=newpassword456&password_confirmation=newpassword456",
            None,
        ),
    )
    .await;
    assert_eq!(unknown, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong_bytes, unknown_bytes, "wrong-kind matches unknown byte-for-byte");

    // Missing token entirely: same page.
    let (status, _, missing_bytes) = send(
        &router,
        form_request(
            "POST",
            "/auth/password",
            "password=newpassword456&password_confirmation=newpassword456",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(missing_bytes, unknown_bytes, "missing token matches unknown byte-for-byte");
}

// ---------------------------------------------------------------------------
// Site texts.
// ---------------------------------------------------------------------------

fn write_site_files(srv: &TestServer) -> Config {
    let dir = srv.dir.path();
    std::fs::write(dir.join("terms.md"), "<p>Be kind.</p>").expect("terms");
    std::fs::write(dir.join("privacy.md"), "<p>We keep nothing.</p>").expect("privacy");
    std::fs::write(dir.join("extended.md"), "A longer story.").expect("extended");
    Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        site_terms_file: Some(dir.join("terms.md")),
        site_privacy_file: Some(dir.join("privacy.md")),
        site_extended_file: Some(dir.join("extended.md")),
        ..Config::default()
    }
}

#[tokio::test]
async fn site_texts_serve_configured_files() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &write_site_files(&srv));

    let (status, _h, bytes) = send(&router, get_request("/api/v1/instance/terms_of_service", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let terms = json(&bytes);
    assert_eq!(terms["content"], "<p>Be kind.</p>");
    assert_eq!(terms["effective"], true);
    assert!(terms["effective_date"].is_string(), "mtime-derived date");
    assert!(terms["succeeded_by"].is_null(), "single file has no version chain");

    // The dated show matches its own effective date and 404s anything else.
    let date = terms["effective_date"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &router,
        get_request(&format!("/api/v1/instance/terms_of_service/{date}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), terms);
    let (status, _, _) = send(
        &router,
        get_request("/api/v1/instance/terms_of_service/1970-01-01", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _h, bytes) = send(&router, get_request("/api/v1/instance/privacy_policy", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let policy = json(&bytes);
    assert_eq!(policy["content"], "<p>We keep nothing.</p>");
    assert!(policy["updated_at"].is_string(), "file mtime");

    let (status, _h, bytes) = send(
        &router,
        get_request("/api/v1/instance/extended_description", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let extended = json(&bytes);
    assert_eq!(extended["content"], "A longer story.");
    assert!(extended["updated_at"].is_string(), "file mtime");
}

#[tokio::test]
async fn site_texts_keep_honest_absence_when_unconfigured() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());

    let (status, _, _) = send(&router, get_request("/api/v1/instance/terms_of_service", None, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &router,
        get_request("/api/v1/instance/terms_of_service/2025-01-01", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(&router, get_request("/api/v1/instance/privacy_policy", None, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _h, bytes) = send(
        &router,
        get_request("/api/v1/instance/extended_description", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes_json(&bytes), serde_json::json!({"updated_at": null, "content": ""}));
}

fn bytes_json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("valid json")
}

// ---------------------------------------------------------------------------
// Rules translations + locale catalog.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rules_render_translations_with_empty_fallback() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());

    let rule = srv
        .store
        .admin_create_rule("Be kind.", "Really, be kind.", now_ms())
        .expect("create rule");
    srv.store
        .admin_set_rule_translation(
            rule.id,
            "fr".to_string(),
            "Soyez gentil.".to_string(),
            "Vraiment.".to_string(),
            now_ms(),
        )
        .expect("set translation");

    let (status, _h, bytes) = send(&router, get_request("/api/v1/instance/rules", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    let row = body.as_array().expect("array").iter().find(|r| r["id"] == rule.id.to_string()).expect("rule row");
    assert_eq!(
        row["translations"],
        serde_json::json!({"fr": {"text": "Soyez gentil.", "hint": "Vraiment."}})
    );

    // A rule with no rows keeps the upstream untranslated shape.
    let bare = srv.store.admin_create_rule("No spam.", "", now_ms()).expect("rule");
    let (status, _h, bytes) = send(&router, get_request("/api/v1/instance/rules", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let row = body.as_array().expect("array").iter().find(|r| r["id"] == bare.id.to_string()).expect("row");
    assert_eq!(row["translations"], serde_json::json!({}));
}

#[tokio::test]
async fn languages_serve_localized_names() {
    let srv = TestServer::new(false);
    let router = ga_router(&srv, &plain_config());
    let (status, _h, bytes) = send(&router, get_request("/api/v1/instance/languages", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let rows = body.as_array().expect("array");
    assert!(rows.len() >= 60, "a real catalog, not a stub (got {})", rows.len());
    let get = |code: &str| {
        rows.iter().find(|r| r["code"] == code).unwrap_or_else(|| panic!("catalog has {code}"))["name"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(get("fr"), "Français");
    assert_eq!(get("de"), "Deutsch");
    assert_eq!(get("en"), "English");
    assert_eq!(get("ja"), "日本語");
    assert_eq!(get("es"), "Español");
    assert_eq!(get("zh-CN"), "简体中文");
    for row in rows {
        let code = row["code"].as_str().expect("code");
        let name = row["name"].as_str().expect("name");
        assert_ne!(name, code, "{code} renders a localized name, not the code");
    }
    let codes: Vec<&str> = rows.iter().map(|r| r["code"].as_str().unwrap()).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    assert_eq!(codes, sorted, "catalog sorts by code");
}

// ---------------------------------------------------------------------------
// Profile namespace + identity proofs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn profile_read_matches_verify_credentials() {
    let srv = TestServer::new(true);
    let router = ga_router(&srv, &plain_config());
    let token = register_user(&srv, "profilevera").await;

    let (v_status, _h, v_bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    assert_eq!(v_status, StatusCode::OK);
    let (p_status, _h, p_bytes) =
        send(&router, get_request("/api/v1/profile", Some(&token), None)).await;
    assert_eq!(p_status, StatusCode::OK, "{}", String::from_utf8_lossy(&p_bytes));
    assert_eq!(json(&p_bytes), json(&v_bytes), "profile read is verify_credentials");

    let (status, _, _) = send(&router, get_request("/api/v1/profile", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn profile_update_matches_update_credentials() {
    let srv = TestServer::new(true);
    let router = ga_router(&srv, &plain_config());
    let token = register_user(&srv, "profilepat").await;

    let body = "display_name=Pat&note=Hello+there&locked=true";
    let (status, _h, bytes) =
        send(&router, form_request("PATCH", "/api/v1/profile", body, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let updated = json(&bytes);
    assert_eq!(updated["display_name"], "Pat");
    assert_eq!(updated["note"], "Hello there");
    assert_eq!(updated["locked"], true);

    // The same patch through `update_credentials` is accepted identically
    // (shared core): `discoverable=false` lands on both reads.
    let (status, _h, _) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "discoverable=false",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, bytes) =
        send(&router, get_request("/api/v1/profile", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["discoverable"], false);
    assert_eq!(json(&bytes)["display_name"], "Pat", "profile writes persist");
}

fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(4, 3, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 60 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).expect("png");
    cursor.into_inner()
}

#[tokio::test]
async fn profile_avatar_delete_clears_upload() {
    let srv = TestServer::new(true);
    let router = ga_router(&srv, &plain_config());
    let token = register_user(&srv, "profileava").await;

    // Deleting with no avatar is still a 200 (upstream parity).
    let request = Request::builder()
        .method("DELETE")
        .uri("/api/v1/profile/avatar")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _h, bytes) = send(&router, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // Upload through PATCH /profile, then delete: the avatar falls back to
    // the missing-asset URL.
    let boundary = "ga-avatar-boundary";
    let mut multipart = Vec::new();
    multipart.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"avatar\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    multipart.extend_from_slice(&png_bytes());
    multipart.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method("PATCH")
        .uri("/api/v1/profile")
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(multipart))
        .unwrap();
    let (status, _h, bytes) = send(&router, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let with_avatar = json(&bytes)["avatar"].as_str().unwrap().to_string();
    assert!(with_avatar.contains("system/media_attachments"), "staged avatar serves: {with_avatar}");

    for (uri, key) in [
        ("/api/v1/profile/avatar", "avatar"),
        ("/api/v1/profile/header", "header"),
    ] {
        let request = Request::builder()
            .method("DELETE")
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, _h, bytes) = send(&router, request).await;
        assert_eq!(status, StatusCode::OK, "delete {uri}: {}", String::from_utf8_lossy(&bytes));
        let body = json(&bytes);
        assert!(
            body[key].as_str().unwrap().contains("missing"),
            "{key} falls back to the missing asset"
        );
    }
}

#[tokio::test]
async fn identity_proofs_always_empty_array() {
    let srv = TestServer::new(true);
    let router = ga_router(&srv, &plain_config());
    let token = register_user(&srv, "proofpam").await;
    let account_id = srv
        .store
        .account_by_handle("proofpam", "example.social")
        .expect("lookup")
        .map(|a| a.id.to_string())
        .expect("account");

    let (status, _h, bytes) = send(
        &router,
        get_request(
            &format!("/api/v1/accounts/{account_id}/identity_proofs"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!([]));

    // Unknown ids 404; anonymous callers are refused like upstream (user token).
    let (status, _, _) = send(
        &router,
        get_request("/api/v1/accounts/99999999999999999/identity_proofs", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &router,
        get_request(&format!("/api/v1/accounts/{account_id}/identity_proofs"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
