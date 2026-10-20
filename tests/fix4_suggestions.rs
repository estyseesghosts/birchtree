//! Fix-round P1/S1 + P3: honest follow suggestions, profile directory and
//! OAuth userinfo. No PII or secrets: all accounts are synthetic (`mk_*`).

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use birchtree::api::mastodon::ApiState;
use birchtree::application::accounts::MemoryInviteStore;
use birchtree::application::auth;
use birchtree::application::context::Context;
use birchtree::application::ports::{FollowOptions, OAuthStore, RelationshipStore, Store};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator};

use common::{TestServer, form_request, get_request, json, send};

const SCOPES: &str = "read write profile follow read:accounts write:accounts \
read:follows write:follows read:blocks write:blocks read:mutes write:mutes";

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

async fn register_app(srv: &TestServer) -> serde_json::Value {
    let body = format!(
        "client_name=Fix4+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(SCOPES),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// Mint a user token with exactly `scopes` (operator bootstrap path).
fn user_token(srv: &TestServer, app: &serde_json::Value, account: Id, scopes: &str) -> String {
    let app_id: Id = app["id"].as_str().unwrap().parse().unwrap();
    let oauth: &dyn OAuthStore = &*srv.store;
    let (plaintext, _) = auth::issue_user_token(
        oauth,
        app_id,
        account,
        scopes.split(' ').map(str::to_string).collect(),
        common::T0,
    )
    .expect("issue user token");
    plaintext
}

/// App-only token (no user) via the client-credentials grant.
async fn app_token(srv: &TestServer, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

fn mk(srv: &TestServer, username: &str) -> Id {
    common::mk_account(&srv.store, username).id
}

fn follow(srv: &TestServer, follower: Id, followed: Id) {
    let rel: &dyn RelationshipStore = &*srv.store;
    rel.follow(
        follower,
        followed,
        FollowOptions { show_reblogs: true, notify: false, languages: None },
        false,
        common::T0,
    )
    .expect("follow");
}

fn block(srv: &TestServer, account: Id, target: Id) {
    let rel: &dyn RelationshipStore = &*srv.store;
    rel.block(account, target, common::T0).expect("block");
}

fn mute(srv: &TestServer, account: Id, target: Id) {
    let rel: &dyn RelationshipStore = &*srv.store;
    rel.mute(account, target, 0, false, common::T0).expect("mute");
}

fn account_ids(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .expect("suggestion array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("account id").to_string())
        .collect()
}

/// `ApiState` over the test server's store, for the handlers whose routes the
/// parent has not wired yet (directory, userinfo): exercised through a
/// test-local router with the real extractors.
fn api_state(srv: &TestServer) -> ApiState {
    use birchtree::streaming::{Hub, Limits};
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let server = common::server_info();
    let store_dyn: Arc<dyn Store> = srv.store.clone();
    let blobs: Arc<dyn birchtree::application::ports::BlobStore> = srv.blobs.clone();
    let streams = Hub::new(store_dyn.clone(), clock.clone(), server.clone(), Limits::default());
    let context = Arc::new(Context::new(
        clock,
        ids,
        store_dyn,
        blobs,
        Arc::new(streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let cfg = Config { public_url: common::TEST_PUBLIC_URL.to_string(), ..Config::default() };
    let _ = cfg;
    ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams,
        federation,
        rate_limit_multiplier: 1,
        mail_env: Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: Arc::new(MemoryInviteStore::default()),
    }
}

fn extra_router(state: ApiState) -> axum::Router {
    use birchtree::api::mastodon::endpoints::directory;
    use birchtree::api::oauth::userinfo;
    axum::Router::new()
        .route("/api/v1/directory", axum::routing::get(directory::show))
        .route(
            "/oauth/userinfo",
            axum::routing::get(userinfo::show).post(userinfo::show),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Suggestions: real ranking, honest shape.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v1_serves_ranked_accounts_with_deprecation() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let viewer = mk(&srv, "viewer1");
    let popular = mk(&srv, "popular1");
    let plain = mk(&srv, "plain1");
    let g1 = mk(&srv, "fan1a");
    let g2 = mk(&srv, "fan1b");
    follow(&srv, g1, popular);
    follow(&srv, g2, popular);

    let token = user_token(&srv, &app, viewer, SCOPES);
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/suggestions?limit=10", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    // Upstream `deprecate_api '2021-05-16'` renders `@<unix>`.
    assert_eq!(
        headers.get("deprecation").expect("deprecation header").to_str().unwrap(),
        "@1621123200"
    );
    let body = json(&bytes);
    let ids = account_ids(&body);
    assert!(!ids.is_empty(), "ranking source must be non-empty: {body}");
    // The viewer is never suggested; the most-followed local account ranks first.
    assert!(!ids.contains(&viewer.to_string()), "{body}");
    assert_eq!(ids[0], popular.to_string(), "{body}");
    assert!(ids.contains(&plain.to_string()), "{body}");
    // Bare account objects (v1), with real counters.
    assert_eq!(body[0]["username"], "popular1");
    assert_eq!(body[0]["followers_count"], 2);
    assert!(body[0].get("source").is_none(), "{body}");
}

#[tokio::test]
async fn v2_suggestion_objects_carry_sources() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let viewer = mk(&srv, "viewer2");
    let bridge = mk(&srv, "bridge2");
    let friend = mk(&srv, "friend2");
    let popular = mk(&srv, "popular2");
    let g1 = mk(&srv, "fan2a");
    let g2 = mk(&srv, "fan2b");
    follow(&srv, viewer, bridge);
    follow(&srv, bridge, friend);
    follow(&srv, g1, popular);
    follow(&srv, g2, popular);

    let token = user_token(&srv, &app, viewer, SCOPES);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/suggestions?limit=10", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    let entries = body.as_array().expect("suggestion array");
    assert!(!entries.is_empty(), "{body}");

    let by_id = |id: Id| {
        entries
            .iter()
            .find(|e| e["account"]["id"] == id.to_string())
            .unwrap_or_else(|| panic!("missing suggestion for {id}: {body}"))
            .clone()
    };
    // No v1 `Deprecation` header on v2.
    let fof = by_id(friend);
    assert_eq!(fof["sources"], serde_json::json!(["friends_of_friends"]), "{body}");
    assert_eq!(fof["source"], "past_interactions", "{body}");
    let top = by_id(popular);
    assert_eq!(top["sources"], serde_json::json!(["global"]), "{body}");
    assert_eq!(top["source"], "global", "{body}");
    assert_eq!(top["account"]["followers_count"], 2);
    // Friends-of-friends rank before the global fill.
    let pos = |id: Id| entries.iter().position(|e| e["account"]["id"] == id.to_string()).unwrap();
    assert!(pos(friend) < pos(popular), "{body}");
    // The bridge (already followed) is never suggested.
    assert!(entries.iter().all(|e| e["account"]["id"] != bridge.to_string()), "{body}");
}

#[tokio::test]
async fn suggestions_exclude_followed_blocked_muted() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let viewer = mk(&srv, "viewer3");
    let followed = mk(&srv, "followed3");
    let blocked = mk(&srv, "blocked3");
    let muted = mk(&srv, "muted3");
    let stranger = mk(&srv, "stranger3");
    follow(&srv, viewer, followed);
    block(&srv, viewer, blocked);
    mute(&srv, viewer, muted);

    let token = user_token(&srv, &app, viewer, SCOPES);
    for uri in ["/api/v1/suggestions?limit=20", "/api/v2/suggestions?limit=20"] {
        let (status, _h, bytes) =
            send(&srv.router, get_request(uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {}", String::from_utf8_lossy(&bytes));
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(text.contains(&stranger.to_string()), "{uri}: {text}");
        for excluded in [followed, blocked, muted, viewer] {
            assert!(!text.contains(&excluded.to_string()), "{uri} leaks {excluded}: {text}");
        }
    }
}

#[tokio::test]
async fn deletes_answer_200_empty_object() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let viewer = mk(&srv, "viewer4");
    let target = mk(&srv, "target4");
    let token = user_token(&srv, &app, viewer, SCOPES);

    // Dismissal persists: the target is suggested before, gone after.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/suggestions?limit=20", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(String::from_utf8_lossy(&bytes).contains(&target.to_string()));

    let uri = format!("/api/v1/suggestions/{target}");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &uri, "", Some(&token)),
    )
    .await;
    // Upstream `render_empty`: 200 `{}`, never 204.
    assert_eq!(status, StatusCode::OK, "{uri}: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!({}), "{uri}");

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/suggestions?limit=20", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&bytes).contains(&target.to_string()));

    // Garbage id 404s.
    let (status, _h, _) = send(
        &srv.router,
        form_request("DELETE", "/api/v1/suggestions/abc", "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // v2 exposes no destroy route upstream: Axum answers 405 (path matches
    // GET, method does not) where Rails answers 404 (no route at all).
    // Recorded in the manifest; both are client-visible refusals.
    let (status, _h, _) = send(
        &srv.router,
        form_request("DELETE", "/api/v2/suggestions", "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    // A read-only token cannot dismiss.
    let reader = user_token(&srv, &app, viewer, "read read:accounts");
    let (status, _h, _) = send(
        &srv.router,
        form_request("DELETE", &uri, "", Some(&reader)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn suggestions_auth_gates_match_upstream() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let viewer = mk(&srv, "viewer5");

    // Anonymous: 401.
    let (status, _h, _) = send(&srv.router, get_request("/api/v1/suggestions", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Wrong scope: 403.
    let writer = user_token(&srv, &app, viewer, "write write:accounts");
    let (status, _h, _) =
        send(&srv.router, get_request("/api/v1/suggestions", Some(&writer), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // App-only token (no user): refused like upstream `require_user!`.
    let app_only = app_token(&srv, &app, "read read:accounts").await;
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/suggestions", Some(&app_only), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
}

// ---------------------------------------------------------------------------
// Profile directory (test-local router until the parent wires the route).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn directory_lists_accounts_anonymously() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let alice = mk(&srv, "diralice");
    let bob = mk(&srv, "dirbob");
    let cara = mk(&srv, "dircara");
    let router = extra_router(api_state(&srv));

    // Anonymous read: bare account array, no auth required.
    let (status, _h, bytes) = send(&router, get_request("/api/v1/directory", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    let ids = account_ids(&body);
    assert!(ids.contains(&alice.to_string()), "{body}");
    assert!(ids.contains(&bob.to_string()), "{body}");
    assert!(ids.contains(&cara.to_string()), "{body}");
    assert_eq!(body[0]["username"].as_str().unwrap().len() > 0, true);

    // `order=new` sorts by id desc.
    let (status, _h, bytes) =
        send(&router, get_request("/api/v1/directory?order=new&limit=10", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let ids = account_ids(&json(&bytes));
    let mut sorted = ids.clone();
    sorted.sort_by(|a, b| b.parse::<u64>().unwrap().cmp(&a.parse::<u64>().unwrap()));
    assert_eq!(ids, sorted);

    // An authenticated viewer excludes blocked accounts; anonymous sees them.
    let viewer = mk(&srv, "dirviewer");
    block(&srv, viewer, cara);
    let token = user_token(&srv, &app, viewer, SCOPES);
    let (status, _h, bytes) = send(
        &router,
        get_request("/api/v1/directory?limit=20", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let authed = account_ids(&json(&bytes));
    assert!(!authed.contains(&cara.to_string()), "{authed:?}");
}

// ---------------------------------------------------------------------------
// OAuth userinfo (test-local router until the parent wires the route).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn userinfo_returns_oidc_claims() {
    let srv = TestServer::new(false);
    let app = register_app(&srv).await;
    let alice = mk(&srv, "uialice");
    let router = extra_router(api_state(&srv));

    let token = user_token(&srv, &app, alice, "profile");
    for method in ["GET", "POST"] {
        let request = if method == "GET" {
            get_request("/oauth/userinfo", Some(&token), None)
        } else {
            form_request("POST", "/oauth/userinfo", "", Some(&token))
        };
        let (status, _h, bytes) = send(&router, request).await;
        assert_eq!(status, StatusCode::OK, "{method}: {}", String::from_utf8_lossy(&bytes));
        let body = json(&bytes);
        assert_eq!(body["iss"], "https://example.social/", "{body}");
        assert_eq!(body["sub"], format!("https://example.social/users/uialice"), "{body}");
        assert_eq!(body["preferred_username"], "uialice", "{body}");
        assert_eq!(body["profile"], "https://example.social/@uialice", "{body}");
        assert!(
            body["picture"].as_str().unwrap().contains("avatars/original/missing.png"),
            "{body}"
        );
        assert!(body.get("name").is_some(), "{body}");
    }

    // A token without the `profile` scope is refused.
    let reader = user_token(&srv, &app, alice, "read read:accounts");
    let (status, _h, _) =
        send(&router, get_request("/oauth/userinfo", Some(&reader), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Anonymous: 401.
    let (status, _h, _) = send(&router, get_request("/oauth/userinfo", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
