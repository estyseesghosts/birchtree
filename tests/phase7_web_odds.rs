//! Phase 7 web/first-party + stray-row contract tests (Subagent B).
//!
//! - `/api/web/settings` with session+CSRF auth (401 without a session,
//!   422 without/a bad CSRF token, 200 `{}` on sync, JSON and form bodies).
//! - `/api/web/embeds/:id` (local public 200 OEmbed; missing/private 404;
//!   remote 401 without a session, 404 with one — no fetch pass exists).
//! - `/api/web/push_subscriptions` CRUD (200 serializer, update, destroy,
//!   always-200 destroy).
//! - `GET /api/v1/timelines/link` (approved URL 200, unknown/blank 404,
//!   scope and auth behavior).
//! - `GET /api/v1/apps/verify_credentials` (user and app-only tokens 200
//!   without the secret; invalid 401).
//! - Relation reads (blocks/mutes/favourites/bookmarks + familiar
//!   followers + featured/followed tags + tag follow/feature): real rows,
//!   `Link` cursors, 401/403/422 paths.
//! - Bundle serving: 404 when unconfigured (and for traversal).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};

use activitypub_federation::http_signatures::generate_actor_keypair;

use birchtree::application::federation_inbound as inbound;
use birchtree::application::federation_inbound::RemoteNote;
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::TrendStore;
use birchtree::domain::{Id, Visibility};
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

const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts \
read:follows write:follows read:statuses write:statuses read:favourites \
write:favourites read:bookmarks write:bookmarks read:blocks write:blocks \
read:mutes write:mutes push";

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

async fn register_account(srv: &TestServer, app: &serde_json::Value, username: &str) -> String {
    let app_tok = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app_tok))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// Log in through the browser flow; returns `(session_cookie, csrf_cookie)`.
async fn login(srv: &TestServer, app: &serde_json::Value, username: &str) -> (String, String) {
    let params = format!(
        "client_id={}&redirect_uri={}&response_type=code&scope=read&state=xyz",
        enc(app["client_id"].as_str().unwrap()),
        enc("https://client.example/cb"),
    );
    let body = format!("username={}&password=password123&{params}", enc(username));
    let (status, headers, bytes) =
        send(&srv.router, form_request("POST", "/oauth/authorize", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "login: {}", String::from_utf8_lossy(&bytes));
    let mut session = None;
    let mut csrf = None;
    for value in headers.get_all("set-cookie") {
        let text = value.to_str().unwrap().to_string();
        let first = text.split(';').next().unwrap().to_string();
        if first.starts_with("birchtree_session=") {
            session = Some(first);
        } else if first.starts_with("birchtree_csrf=") {
            csrf = Some(first);
        }
    }
    (
        session.expect("session cookie"),
        csrf.expect("csrf cookie"),
    )
}

fn web_request(
    method: &str,
    uri: &str,
    body: &str,
    content_type: &str,
    cookies: Option<(&str, &str)>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header("content-type", content_type);
    if let Some((session, csrf_cookie)) = cookies {
        builder = builder.header("cookie", format!("{session}; {csrf_cookie}"));
    }
    if let Some(token) = csrf {
        builder = builder.header("x-csrf-token", token);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn csrf_value(cookie: &str) -> &str {
    cookie.split_once('=').map(|(_, v)| v).unwrap_or("")
}

async fn account_id(srv: &TestServer, token: &str) -> String {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

async fn post_status(srv: &TestServer, token: &str, body: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "post: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// --- /api/web/settings ----------------------------------------------------

#[tokio::test]
async fn web_settings_require_session_and_csrf() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let _alice = register_account(&srv, &app, "alice").await;
    let (session, csrf_cookie) = login(&srv, &app, "alice").await;
    let csrf = csrf_value(&csrf_cookie).to_string();

    // No session at all.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("PUT", "/api/web/settings", "data={}", "application/x-www-form-urlencoded", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{}", String::from_utf8_lossy(&bytes));

    // Session but no CSRF header.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request(
            "PUT", "/api/web/settings", "data={}",
            "application/x-www-form-urlencoded",
            Some((&session, &csrf_cookie)), None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        json(&bytes)["error"],
        serde_json::json!("Can't verify CSRF token authenticity.")
    );

    // Wrong CSRF value.
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request(
            "PUT", "/api/web/settings", "data={}",
            "application/x-www-form-urlencoded",
            Some((&session, &csrf_cookie)), Some("wrong"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Correct pair: 200 `{}` for PUT and PATCH, form and JSON bodies.
    for method in ["PUT", "PATCH"] {
        let (status, _h, bytes) = send(
            &srv.router,
            web_request(
                method, "/api/web/settings",
                &format!("data={}", enc(r#"{"policy":"none"}"#)),
                "application/x-www-form-urlencoded",
                Some((&session, &csrf_cookie)), Some(&csrf),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method}: {}", String::from_utf8_lossy(&bytes));
        assert_eq!(json(&bytes), serde_json::json!({}));
    }
    let (status, _h, bytes) = send(
        &srv.router,
        web_request(
            "PUT", "/api/web/settings", r#"{"data":{"policy":"all"}}"#,
            "application/json",
            Some((&session, &csrf_cookie)), Some(&csrf),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // Invalid data is a 422, not a silent accept.
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request(
            "PUT", "/api/web/settings", "data=%5B1%2C2",
            "application/x-www-form-urlencoded",
            Some((&session, &csrf_cookie)), Some(&csrf),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// --- /api/web/embeds -------------------------------------------------------

#[tokio::test]
async fn web_embeds_local_remote_and_auth_paths() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice_tok = register_account(&srv, &app, "alice").await;
    let (session, csrf_cookie) = login(&srv, &app, "alice").await;
    let cookies = format!("{session}; {csrf_cookie}");

    let public = post_status(&srv, &alice_tok, "status=hello+world&visibility=public").await;
    let public_id = public["id"].as_str().unwrap().to_string();
    let private = post_status(&srv, &alice_tok, "status=secret&visibility=private").await;
    let private_id = private["id"].as_str().unwrap().to_string();

    // Anonymous local public: 200 OEmbed.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/web/embeds/{public_id}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], serde_json::json!("rich"));
    assert_eq!(body["version"], serde_json::json!("1.0"));
    assert_eq!(body["author_name"], serde_json::json!("alice"));
    assert!(body["html"].as_str().unwrap().contains(&public_id));

    // Missing and private statuses are 404, never leaked.
    for id in [private_id.as_str(), "999999999"] {
        let (status, _h, _bytes) = send(
            &srv.router,
            get_request(&format!("/api/web/embeds/{id}"), None, None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "id {id}");
    }

    // Non-numeric ids are 404.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/web/embeds/nope", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The author may embed their own private status over the session.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/web/embeds/{private_id}"), None, Some(&cookies)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // A remote status without a session is 401; with a session the
    // (unavailable) fetch answers 404 rather than fabricated JSON.
    let keys = generate_actor_keypair().expect("keypair");
    let upsert = RemoteAccountUpsert {
        actor_uri: "https://remote.example/users/eve".into(),
        username: "eve".into(),
        domain: "remote.example".into(),
        display_name: String::new(),
        note: String::new(),
        locked: false,
        inbox_uri: "https://remote.example/users/eve/inbox".into(),
        shared_inbox_uri: Some("https://remote.example/inbox".into()),
        public_key_pem: keys.public_key,
        key_id: "https://remote.example/users/eve#main-key".into(),
    };
    let eve = srv.store.upsert_remote(upsert, 1_700_000_000_000).expect("remote");
    let note = RemoteNote {
        uri: "https://remote.example/notes/1".into(),
        url: "https://remote.example/notes/1".into(),
        author_id: eve.id,
        published_ms: 1_700_000_000_000,
        visibility: Visibility::Public,
        content_html: "<p>hi</p>".into(),
        spoiler_text: None,
        sensitive: false,
        language: None,
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        quote_uri: None,
        poll: None,
    };
    let ingested = inbound::ingest_note(&*srv.store, note, None, 1_700_000_000_000).expect("ingest");
    let remote_id = ingested.status.id.to_string();
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/web/embeds/{remote_id}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/web/embeds/{remote_id}"), None, Some(&cookies)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{}", String::from_utf8_lossy(&bytes));
}

// --- /api/web/push_subscriptions -------------------------------------------

fn push_form(endpoint: &str) -> String {
    format!(
        "subscription%5Bendpoint%5D={}&subscription%5Bkeys%5D%5Bp256dh%5D={}&subscription%5Bkeys%5D%5Bauth%5D={}&subscription%5Bstandard%5D=true&data%5Bpolicy%5D=all",
        enc(endpoint),
        enc("p256dh-key"),
        enc("auth-key"),
    )
}

#[tokio::test]
async fn web_push_subscription_crud() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let _alice = register_account(&srv, &app, "alice").await;
    let (session, csrf_cookie) = login(&srv, &app, "alice").await;
    let csrf = csrf_value(&csrf_cookie).to_string();
    let pair = Some((session.as_str(), csrf_cookie.as_str()));

    // CSRF applies here too.
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request("POST", "/api/web/push_subscriptions", &push_form("https://push.example/a"), "application/x-www-form-urlencoded", pair, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Validation: blank endpoint is a 422.
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request("POST", "/api/web/push_subscriptions", &push_form(""), "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Create.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("POST", "/api/web/push_subscriptions", &push_form("https://push.example/a"), "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let created = json(&bytes);
    assert_eq!(created["endpoint"], serde_json::json!("https://push.example/a"));
    assert_eq!(created["policy"], serde_json::json!("all"));
    assert_eq!(created["standard"], serde_json::json!(true));
    assert!(created["id"].as_str().is_some());
    let id = created["id"].as_str().unwrap().to_string();

    // Update the data payload (PUT and PATCH aliases).
    for method in ["PUT", "PATCH"] {
        let (status, _h, bytes) = send(
            &srv.router,
            web_request(
                method, &format!("/api/web/push_subscriptions/{id}"),
                "data%5Bpolicy%5D=none", "application/x-www-form-urlencoded",
                pair, Some(&csrf),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method}: {}", String::from_utf8_lossy(&bytes));
        assert_eq!(json(&bytes)["policy"], serde_json::json!("none"));
    }

    // Destroy by id, then destroy of an unknown token: always 200.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("DELETE", &format!("/api/web/push_subscriptions/{id}"), "", "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request("DELETE", "/api/web/push_subscriptions/does-not-exist", "", "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// --- /api/v1/timelines/link -------------------------------------------------

#[tokio::test]
async fn timelines_link_needs_an_approved_link() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice_tok = register_account(&srv, &app, "alice").await;
    let status = post_status(
        &srv, &alice_tok,
        &format!("status={}&visibility=public", enc("read https://example.com/article today")),
    )
    .await;
    let status_id = status["id"].as_str().unwrap().to_string();

    // Unknown and blank URLs are 404 (upstream's `find_by!`).
    for query in ["url=https%3A%2F%2Fexample.com%2Fnope", ""] {
        let (status, _h, _bytes) = send(
            &srv.router,
            get_request(&format!("/api/v1/timelines/link?{query}"), Some(&alice_tok), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "query {query}");
    }

    // An unreviewed link row is still 404.
    srv.store
        .ensure_trend_links(&["https://example.com/article".to_string()], 1_700_000_000_000)
        .expect("ensure link");
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(
            "/api/v1/timelines/link?url=https%3A%2F%2Fexample.com%2Farticle",
            Some(&alice_tok), None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Approved: the linking status shows up, anonymously too.
    let alice_id = account_id(&srv, &alice_tok).await.parse::<Id>().unwrap();
    let rows = srv.store.ensure_trend_links(&["https://example.com/article".to_string()], 1_700_000_000_001).expect("ensure");
    srv.store
        .review_trend_link(rows[0].id, true, alice_id, 1_700_000_000_002)
        .expect("approve");
    for token in [Some(alice_tok.clone()), None] {
        let (status, _h, bytes) = send(
            &srv.router,
            get_request(
                "/api/v1/timelines/link?url=https%3A%2F%2Fexample.com%2Farticle",
                token.as_deref(), None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        let body = json(&bytes);
        assert!(body.as_array().unwrap().iter().any(|s| s["id"] == status_id));
    }

    // A token without the read scope is refused.
    let narrow = app_token(&srv, &app, "write:statuses").await;
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(
            "/api/v1/timelines/link?url=https%3A%2F%2Fexample.com%2Farticle",
            Some(&narrow), None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --- /api/v1/apps/verify_credentials ----------------------------------------

#[tokio::test]
async fn apps_verify_credentials_shows_app_without_secret() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice_tok = register_account(&srv, &app, "alice").await;

    // App-only token.
    let app_tok = app_token(&srv, &app, "read").await;
    for token in [&app_tok, &alice_tok] {
        let (status, _h, bytes) = send(
            &srv.router,
            get_request("/api/v1/apps/verify_credentials", Some(token), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        let body = json(&bytes);
        assert_eq!(body["name"], serde_json::json!("Test App"));
        assert!(body.get("client_secret").is_none(), "{body}");
        assert!(body.get("client_secret_expires_at").is_none(), "{body}");
    }

    // Invalid token is 401.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/v1/apps/verify_credentials", Some("nope"), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// --- relation collection reads ----------------------------------------------

async fn setup_pair(srv: &TestServer) -> (String, String, String, String) {
    let app = register_app(srv, ALL_SCOPES).await;
    let alice = register_account(srv, &app, "alice").await;
    let bob = register_account(srv, &app, "bob").await;
    let alice_id = account_id(srv, &alice).await;
    let bob_id = account_id(srv, &bob).await;
    (app.to_string(), alice, bob, format!("{alice_id}/{bob_id}"))
}

#[tokio::test]
async fn relation_blocks_mutes_lists() {
    let srv = TestServer::new(true);
    let (_app, alice, _bob, ids) = setup_pair(&srv).await;
    let mut parts = ids.split('/');
    let _alice_id = parts.next().unwrap().to_string();
    let bob_id = parts.next().unwrap().to_string();

    // Empty tables read empty (never literals).
    for path in ["/api/v1/blocks", "/api/v1/mutes"] {
        let (status, headers, bytes) =
            send(&srv.router, get_request(path, Some(&alice), None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(json(&bytes), serde_json::json!([]));
        assert!(headers.get("link").is_none(), "no cursor on empty page");
    }

    // Auth: missing token 401, app-only token 422, wrong scope 403.
    let (status, _h, _bytes) =
        send(&srv.router, get_request("/api/v1/blocks", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Block then mute; both lists show the account with Link cursors.
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/block"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/blocks", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], serde_json::json!(bob_id));

    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/mute"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers, bytes) =
        send(&srv.router, get_request("/api/v1/mutes", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], serde_json::json!(bob_id));
    assert!(body[0].get("mute_expires_at").is_some(), "{body}");
    assert!(headers.get("link").is_some(), "cursor on non-empty page");
}

#[tokio::test]
async fn relation_favourites_bookmarks_lists() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice = register_account(&srv, &app, "alice").await;
    let bob = register_account(&srv, &app, "bob").await;

    let status = post_status(&srv, &bob, "status=bookmark+me&visibility=public").await;
    let status_id = status["id"].as_str().unwrap().to_string();

    // Empty reads.
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/favourites", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!([]));

    // Auth paths.
    let (status, _h, _bytes) =
        send(&srv.router, get_request("/api/v1/favourites", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Favourite + bookmark, then list.
    for (action, path) in [("favourite", "/api/v1/favourites"), ("bookmark", "/api/v1/bookmarks")] {
        let (status, _h, _bytes) = send(
            &srv.router,
            form_request("POST", &format!("/api/v1/statuses/{status_id}/{action}"), "", Some(&alice)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{action}");
        let (status, headers, bytes) =
            send(&srv.router, get_request(path, Some(&alice), None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let body = json(&bytes);
        assert_eq!(body.as_array().unwrap().len(), 1, "{body}");
        assert_eq!(body[0]["id"], serde_json::json!(status_id));
        assert!(headers.get("link").is_some(), "{path} cursor");
    }
}

#[tokio::test]
async fn familiar_followers_lists_mutuals() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice = register_account(&srv, &app, "alice").await;
    let bob = register_account(&srv, &app, "bob").await;
    let carol = register_account(&srv, &app, "carol").await;
    let bob_id = account_id(&srv, &bob).await;
    let carol_id = account_id(&srv, &carol).await;

    // Alice follows Bob and Carol; Bob follows Carol.
    for (token, target) in [(&alice, &bob_id), (&alice, &carol_id), (&bob, &carol_id)] {
        let (status, _h, _bytes) = send(
            &srv.router,
            form_request("POST", &format!("/api/v1/accounts/{target}/follow"), "", Some(token)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let uri = format!("/api/v1/accounts/familiar_followers?id[]={bob_id}");
    let (status, _h, bytes) =
        send(&srv.router, get_request(&uri, Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], serde_json::json!(bob_id));
    let familiars: Vec<String> = body[0]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(familiars, vec![carol_id]);

    // Unknown ids are skipped; missing auth is 401.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/accounts/familiar_followers?id[]={bob_id}&id[]=999999999"),
            Some(&alice), None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&uri, None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// --- tags --------------------------------------------------------------------

#[tokio::test]
async fn tags_follow_feature_and_featured_reads() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice = register_account(&srv, &app, "alice").await;
    let alice_id = account_id(&srv, &alice).await;

    // Invalid names are 404.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/v1/tags/%3F%3F%3F", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Show starts unfollowed/unfeatured.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/tags/rust", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["following"], serde_json::json!(false));

    // Follow + feature.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/tags/rust/follow", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["following"], serde_json::json!(true));
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/tags/rust/feature", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Followed tags and featured tags read the rows back.
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/followed_tags", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes).as_array().unwrap().iter().any(|t| t["name"] == "rust"));

    // A tagged status feeds the featured usage count.
    post_status(&srv, &alice, &format!("status={}&visibility=public", enc("i love #rust"))).await;
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/featured_tags", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    let featured = body.as_array().unwrap().iter().find(|t| t["name"] == "rust").expect("featured");
    assert_eq!(featured["statuses_count"], serde_json::json!("1"));
    let row_id = featured["id"].as_str().unwrap().to_string();

    // Per-account featured tags are public.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{alice_id}/featured_tags"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(json(&bytes).as_array().unwrap().iter().any(|t| t["name"] == "rust"));

    // Unfollow + unfeature + destroy.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/tags/rust/unfollow", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["following"], serde_json::json!(false));
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/tags/rust/unfeature", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/featured_tags/{row_id}"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/featured_tags", Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!([]));

    // Featured create validates the name.
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/featured_tags", "name=%3F%3F", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- bundle ------------------------------------------------------------------

#[tokio::test]
async fn bundle_serving_404s_when_unconfigured() {
    let srv = TestServer::new(true);
    for path in ["/app/", "/app/index.html", "/app/../secret"] {
        let (status, _h, _bytes) = send(&srv.router, get_request(path, None, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}
