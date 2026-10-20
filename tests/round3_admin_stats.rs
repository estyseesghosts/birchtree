//! Round 3, area E (admin stats completeness): every admin key served or
//! refused-with-reason, never zero-filled or fabricated.
//!
//! Upstream pins (Mastodon v4.7.3, `8ff11c2b`):
//! - NodeInfo: `NodeInfo::Serializer#usage`
//!   (`app/serializers/node_info/serializer.rb:33`): `users.{total,
//!   active_month, active_halfyear}` from `InstancePresenter#user_count` /
//!   `#active_user_count(4/24)` and `local_posts` from `#status_count`
//!   (`app/presenters/instance_presenter.rb:53-62`).
//! - Measures: `Admin::Metrics::Measure::MEASURES`
//!   (`app/lib/admin/metrics/measure.rb:4-18`); tag measures require
//!   `params.require(key).permit(:id)` (`with_params?`, e.g.
//!   `tag_accounts_measure.rb:5-7`).
//! - Dimensions: `Admin::Metrics::Dimension::DIMENSIONS`
//!   (`app/lib/admin/metrics/dimension.rb:4-14`); `software_versions`
//!   reports the component stack (`software_versions_dimension.rb`),
//!   `space_usage` reports postgresql/redis/media/search
//!   (`space_usage_dimension.rb`).
//! - Retention cohorts are sign-ins (`users.current_sign_in_at`,
//!   `app/lib/admin/metrics/retention.rb:62-89`).
//!
//! Birchtree rule: each key is served from real local counts or stays absent
//! from the response array with its reason in the dashboard note
//! (`HANDOFF_round3_admin-dashboard-note.md`, parent merges into D31/D39).

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};

use birchtree::application::ports::{AccountStore, OAuthStore};
use birchtree::domain::{Clock, Id, SystemClock, UserRole};
use birchtree::persistence::redb::RedbStore;

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses write:statuses read:follows write:follows write:reports admin:read admin:write admin:read:accounts admin:write:accounts admin:read:reports admin:write:reports";

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

/// Mint a user token directly (operator bootstrap path).
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

async fn admin_fixture() -> (TestServer, serde_json::Value, String) {
    let srv = TestServer::new(true);
    let app = register_app(&srv.router, ALL_SCOPES).await;
    let admin = common::mk_account(&srv.store, "boss");
    srv.store
        .set_user_role(admin.id, UserRole::Admin)
        .expect("grant role");
    let admin_token = mint_user_token(&srv.store, &app, admin.id);
    (srv, app, admin_token)
}

#[allow(dead_code)]
fn account_id(srv: &TestServer, name: &str) -> Id {
    srv.store
        .account_by_handle(name, "example.social")
        .expect("lookup")
        .expect("account")
        .id
}

/// Create a local account stamped at the live clock (retention cohorts are
/// creation-day buckets clamped to the trailing 31 days / 12 months, so the
/// fixed `T0` fixture time would fall outside any live window).
fn mk_now_account(srv: &TestServer, username: &str) -> Id {
    let now = Arc::new(SystemClock).now_ms();
    birchtree::application::accounts::create_account(
        &*srv.store,
        "https://example.social",
        now,
        birchtree::application::accounts::CreateAccountCommand {
            username: username.to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
    .id
}

async fn post_status(router: &axum::Router, token: &str, body: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(router, form_request("POST", "/api/v1/statuses", body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "post: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn admin_measures(router: &axum::Router, token: &str, body: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(router, form_request("POST", "/api/v1/admin/measures", body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "measures: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn admin_dimensions(router: &axum::Router, token: &str, body: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(
        router,
        form_request("POST", "/api/v1/admin/dimensions", body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dimensions: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

fn by_key<'a>(rows: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    rows.as_array().unwrap().iter().find(|r| r["key"] == key)
}

/// Numeric admin `:id` of the `birch` trend-tag row from the review queue.
async fn trend_tag_id(router: &axum::Router, token: &str) -> String {
    let (status, _h, bytes) = send(
        router,
        get_request("/api/v1/admin/trends/tags", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rows = json(&bytes);
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "birch")
        .unwrap_or_else(|| panic!("birch candidate: {}", String::from_utf8_lossy(&bytes)))
        ["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// (a) NodeInfo usage.users + usage.localPosts from local counts.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nodeinfo_usage_counts_match_seeded_locals() {
    let (srv, app, admin_token) = admin_fixture().await;
    let alice = common::mk_account(&srv.store, "alice");
    let bob = common::mk_account(&srv.store, "bob");
    let alice_token = mint_user_token(&srv.store, &app, alice.id);
    let bob_token = mint_user_token(&srv.store, &app, bob.id);
    post_status(&srv.router, &alice_token, "status=hello+%23birch").await;
    post_status(&srv.router, &bob_token, "status=world").await;
    // One real password sign-in in the log (round-2 telemetry).
    let now = Arc::new(SystemClock).now_ms();
    let accounts: &dyn AccountStore = &*srv.store;
    accounts.record_sign_in(alice.id, now).expect("sign-in");

    let (status, _, bytes) = send(
        &srv.router,
        Request::builder().uri("/nodeinfo/2.0").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc = json(&bytes);
    assert_eq!(doc["version"], "2.0");
    // Three local accounts (boss/alice/bob), two local posts.
    assert_eq!(doc["usage"]["users"]["total"], 3);
    assert_eq!(doc["usage"]["localPosts"], 2);
    assert!(doc["usage"]["users"]["activeMonth"].as_u64().unwrap() >= 1);
    assert!(doc["usage"]["users"]["activeHalfyear"].as_u64().unwrap() >= 1);

    // The v1 instance counters agree (same local-only scans).
    let (status, _, bytes) = send(&srv.router, get_request("/api/v1/instance", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let instance = json(&bytes);
    assert_eq!(instance["stats"]["user_count"], 3);
    assert_eq!(instance["stats"]["status_count"], 2);

    let _ = admin_token;
}

// ---------------------------------------------------------------------------
// (c) tag measures + dimensions served with `<key>[id]`, refused without.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tag_measures_served_with_id_refused_without() {
    let (srv, app, admin_token) = admin_fixture().await;
    let alice = common::mk_account(&srv.store, "alice");
    let alice_token = mint_user_token(&srv.store, &app, alice.id);
    post_status(&srv.router, &alice_token, "status=tagged+%23birch&language=en").await;
    let tag_id = trend_tag_id(&srv.router, &admin_token).await;

    // Without `<key>[id]>` the keys stay omitted (lenient where upstream
    // answers 400 with `params.require`; reason: REFUSED_TAG_NO_ID).
    let rows = admin_measures(
        &srv.router,
        &admin_token,
        "keys[]=tag_accounts&keys[]=tag_uses&keys[]=tag_servers",
    )
    .await;
    assert!(by_key(&rows, "tag_accounts").is_none());
    assert!(by_key(&rows, "tag_uses").is_none());
    assert!(by_key(&rows, "tag_servers").is_none());

    // An unknown tag id stays omitted too (never zero-filled).
    let body = format!(
        "keys[]=tag_accounts&keys[]=tag_uses&keys[]=tag_servers&tag_accounts%5Bid%5D=999999999999999999&tag_uses%5Bid%5D=999999999999999999&tag_servers%5Bid%5D=999999999999999999"
    );
    let rows = admin_measures(&srv.router, &admin_token, &body).await;
    assert!(by_key(&rows, "tag_accounts").is_none());
    assert!(by_key(&rows, "tag_uses").is_none());
    assert!(by_key(&rows, "tag_servers").is_none());

    // With the id every key is served from the real hashtag index.
    let body = format!(
        "keys[]=tag_accounts&keys[]=tag_uses&keys[]=tag_servers&tag_accounts%5Bid%5D={tag_id}&tag_uses%5Bid%5D={tag_id}&tag_servers%5Bid%5D={tag_id}"
    );
    let rows = admin_measures(&srv.router, &admin_token, &body).await;
    let uses = by_key(&rows, "tag_uses").expect("tag_uses served");
    assert!(uses["total"].as_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert!(uses.get("previous_total").is_some());
    assert!(uses["data"].is_array());
    let accounts = by_key(&rows, "tag_accounts").expect("tag_accounts served");
    assert!(accounts["total"].as_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert!(accounts.get("previous_total").is_some());
    let servers = by_key(&rows, "tag_servers").expect("tag_servers served");
    assert!(servers["total"].as_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert!(servers.get("previous_total").is_some());
}

#[tokio::test]
async fn tag_dimensions_served_with_id() {
    let (srv, app, admin_token) = admin_fixture().await;
    let alice = common::mk_account(&srv.store, "alice");
    let alice_token = mint_user_token(&srv.store, &app, alice.id);
    post_status(&srv.router, &alice_token, "status=tagged+%23birch&language=en").await;
    let tag_id = trend_tag_id(&srv.router, &admin_token).await;

    let body = format!(
        "keys[]=tag_servers&keys[]=tag_languages&tag_servers%5Bid%5D={tag_id}&tag_languages%5Bid%5D={tag_id}"
    );
    let rows = admin_dimensions(&srv.router, &admin_token, &body).await;
    let servers = by_key(&rows, "tag_servers").expect("tag_servers served");
    assert!(
        servers["data"].as_array().unwrap().iter().any(|e| e["key"] == "example.social" && e["value"] == "1"),
        "{servers}"
    );
    let languages = by_key(&rows, "tag_languages").expect("tag_languages served");
    assert!(
        languages["data"].as_array().unwrap().iter().any(|e| e["key"] == "en" && e["value"] == "1"),
        "{languages}"
    );

    // Without the id both stay omitted.
    let rows = admin_dimensions(&srv.router, &admin_token, "keys[]=tag_servers&keys[]=tag_languages").await;
    assert!(by_key(&rows, "tag_servers").is_none());
    assert!(by_key(&rows, "tag_languages").is_none());
}

// ---------------------------------------------------------------------------
// (b)+(d) refused keys stay absent: interactions, software_versions.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refused_keys_stay_absent_with_reasons() {
    let (srv, _app, admin_token) = admin_fixture().await;
    let rows = admin_measures(&srv.router, &admin_token, "keys[]=interactions&keys[]=new_users").await;
    assert!(by_key(&rows, "interactions").is_none(), "no event log: {rows}");
    assert!(by_key(&rows, "new_users").is_some());
    let rows = admin_dimensions(&srv.router, &admin_token, "keys[]=software_versions&keys[]=servers").await;
    assert!(by_key(&rows, "software_versions").is_none(), "no census: {rows}");
    assert!(by_key(&rows, "servers").is_some());
    // The reasons are locked as constants (dashboard note cites them).
    assert!(birchtree::api::mastodon::endpoints::admin_infra::REFUSED_INTERACTIONS.contains("event log"));
    assert!(
        birchtree::api::mastodon::endpoints::admin_infra::REFUSED_SOFTWARE_VERSIONS
            .contains("census")
    );
}

// ---------------------------------------------------------------------------
// (e) retention cohorts from the sign-in log (unioned with authorship).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retention_counts_signin_cohorts() {
    let (srv, _app, admin_token) = admin_fixture().await;
    let alice_id = mk_now_account(&srv, "alice");
    let _bob_id = mk_now_account(&srv, "bob");
    // A sign-in with no post: under authorship-only cohorts this member
    // would be silent; the sign-in log must count them.
    let now = Arc::new(SystemClock).now_ms();
    let accounts: &dyn AccountStore = &*srv.store;
    accounts.record_sign_in(alice_id, now).expect("sign-in");

    let body = format!("start_at={}&end_at={}&frequency=day", now - 86_400_000, now + 60_000);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/admin/retention", &body, Some(&admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let cohorts = json(&bytes);
    let total_active: u64 = cohorts
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["data"].as_array().unwrap().iter())
        .map(|p| p["value"].as_str().unwrap().parse::<u64>().unwrap())
        .sum();
    assert!(total_active >= 1, "sign-in must count: {cohorts}");
}

// ---------------------------------------------------------------------------
// (f) space_usage: media bytes + measured database size.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn space_usage_covers_media_and_database() {
    let (srv, _app, admin_token) = admin_fixture().await;
    let rows = admin_dimensions(&srv.router, &admin_token, "keys[]=space_usage").await;
    let space = by_key(&rows, "space_usage").expect("space_usage served");
    let data = space["data"].as_array().expect("data");
    let media = data.iter().find(|e| e["key"] == "media").expect("media entry");
    assert_eq!(media["unit"], "bytes");
    let media_bytes: u64 = media["value"].as_str().unwrap().parse().unwrap();
    let db = data.iter().find(|e| e["key"] == "database").expect("database entry");
    assert_eq!(db["unit"], "bytes");
    assert!(db["human_value"].is_string());
    let db_bytes: u64 = db["value"].as_str().unwrap().parse().unwrap();
    // Measured from the live redb file: non-empty and covering media.
    assert!(db_bytes > 0, "database size must be measured, not zero");
    assert!(db_bytes >= media_bytes);
    // postgres/redis/search sizes are never fabricated.
    assert!(data.iter().all(|e| e["key"] != "postgresql" && e["key"] != "redis" && e["key"] != "search"));
}
