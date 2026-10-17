//! Phase 4 timeline contract tests: home, public, tag and list feeds.
//!
//! Covers the viewer policy (visibility, block/mute/domain, replies, reblogs),
//! the exact query contract (limit, cursors, `Link`, cache headers, auth), the
//! `filtered` annotation, and bounded-scan evidence from the store metrics.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::Value;

use birchtree::application::ports::AccountStore;

use common::{TestServer, form_request, get_request, json, send};

// ----- media fixtures (multipart, as in phase4_media.rs) -------------------

const BOUNDARY: &str = "birchtree-test-boundary";

/// A valid 3x2 PNG encoded with the server's own image stack.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

/// Upload a PNG as `token` and return the media attachment id.
async fn upload_png(srv: &TestServer, token: &str) -> String {
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"pixel.png\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(&png_bytes());
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/media")
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body))
        .expect("multipart request");
    let (status, _h, bytes) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

const PUBLIC_CACHE: &str = "max-age=15, public, stale-while-revalidate=30, stale-if-error=86400";
const PRIVATE_CACHE: &str = "private, no-store";

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

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=Phase4Timelines&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &Value, scopes: &str) -> String {
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

async fn register_account(srv: &TestServer, app: &Value, username: &str) -> String {
    let app_tok = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// A user token with every scope the timeline routes read or write.
async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(
        srv,
        "read write follow profile read:accounts write:accounts read:lists write:lists read:statuses write:statuses",
    )
    .await;
    register_account(srv, &app, username).await
}

fn account_id(srv: &TestServer, username: &str) -> String {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

/// Form body for a status: encoded text, visibility and extra fields.
fn status_form(text: &str, visibility: &str, extra: &str) -> String {
    let mut body = format!("status={}&visibility={visibility}", enc(text));
    if !extra.is_empty() {
        body.push('&');
        body.push_str(extra);
    }
    body
}

/// POST a form as `token`, asserting 200 and returning the body.
async fn post_ok(srv: &TestServer, path: &str, token: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn create(srv: &TestServer, token: &str, text: &str, visibility: &str) -> String {
    create_with(srv, token, &status_form(text, visibility, "")).await
}

async fn create_with(srv: &TestServer, token: &str, body: &str) -> String {
    post_ok(srv, "/api/v1/statuses", token, body).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn put_ok(srv: &TestServer, path: &str, token: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("PUT", path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "PUT {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn reblog(srv: &TestServer, token: &str, status_id: &str) {
    post_ok(srv, &format!("/api/v1/statuses/{status_id}/reblog"), token, "").await;
}

/// GET a URI and return `(status, headers, parsed body)`.
async fn get(srv: &TestServer, uri: &str, token: Option<&str>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, bytes) = send(&srv.router, get_request(uri, token, None)).await;
    (status, headers, json(&bytes))
}

/// GET a timeline and assert 200, returning the status ids in response order.
async fn ids_of(srv: &TestServer, uri: &str, token: Option<&str>) -> Vec<String> {
    let (status, _h, body) = get(srv, uri, token).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
    ids(&body)
}

fn ids(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap_or_else(|| panic!("array expected: {body}"))
        .iter()
        .map(|status| status["id"].as_str().unwrap().to_string())
        .collect()
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(|v| v.to_str().unwrap().to_string())
}

async fn follow(srv: &TestServer, token: &str, target: &str, form: &str) {
    post_ok(srv, &format!("/api/v1/accounts/{target}/follow"), token, form).await;
}

async fn block(srv: &TestServer, token: &str, target: &str) {
    post_ok(srv, &format!("/api/v1/accounts/{target}/block"), token, "").await;
}

async fn mute(srv: &TestServer, token: &str, target: &str) {
    post_ok(srv, &format!("/api/v1/accounts/{target}/mute"), token, "").await;
}

/// Five users: alice (viewer), bob, carol, dave, eve.
async fn five_users(srv: &TestServer) -> [String; 5] {
    [
        user_token(srv, "alice").await,
        user_token(srv, "bob").await,
        user_token(srv, "carol").await,
        user_token(srv, "dave").await,
        user_token(srv, "eve").await,
    ]
}

// ----- home ----------------------------------------------------------------

#[tokio::test]
async fn home_orders_own_and_followed_statuses_newest_first() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, _dave, _eve] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;

    let a1 = create(&srv, &alice, "alice one", "public").await;
    let b1 = create(&srv, &bob, "bob one", "public").await;
    let c1 = create(&srv, &carol, "carol one", "public").await;
    let a2 = create(&srv, &alice, "alice two", "public").await;
    let b2 = create(&srv, &bob, "bob two", "public").await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(got, vec![b2, a2, b1.clone(), a1.clone()]);
    assert!(!got.contains(&c1), "non-followed author must not appear");
}

#[tokio::test]
async fn home_direct_statuses_reach_only_the_mentioned_viewer() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    follow(&srv, &alice, &account_id(&srv, "carol"), "").await;

    let to_alice = create(&srv, &bob, "@alice hello there", "direct").await;
    let to_carol = create(&srv, &bob, "@carol hello there", "direct").await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert!(got.contains(&to_alice), "direct to viewer must appear: {got:?}");
    assert!(!got.contains(&to_carol), "direct to someone else must not: {got:?}");
}

#[tokio::test]
async fn home_private_statuses_need_a_follow() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, _dave, _eve] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;

    let from_bob = create(&srv, &bob, "bob friends only", "private").await;
    let from_carol = create(&srv, &carol, "carol friends only", "private").await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert!(got.contains(&from_bob), "private from followed must appear");
    assert!(!got.contains(&from_carol), "private from non-followed must not");
}

#[tokio::test]
async fn home_hides_muted_authors_and_reblogs_of_muted_targets() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    follow(&srv, &alice, &account_id(&srv, "dave"), "").await;

    let bob_post = create(&srv, &bob, "bob post", "public").await;
    let carol_post = create(&srv, &carol, "carol post", "public").await;
    let dave_post = create(&srv, &dave, "dave post", "public").await;
    // Bob boosts carol's post; alice mutes carol, so the boost must go too.
    reblog(&srv, &bob, &carol_post).await;
    mute(&srv, &alice, &account_id(&srv, "carol")).await;
    // Dave's own post is fine; bob gets muted below.
    mute(&srv, &alice, &account_id(&srv, "bob")).await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(got, vec![dave_post], "muted bob and muted carol's boost are out");
    assert!(!got.contains(&bob_post));
    assert!(!got.contains(&carol_post));
}

#[tokio::test]
async fn home_excludes_blocked_authors_in_both_directions() {
    // A block also drops the follow (Mastodon's block cascade), so these
    // assert the outcome: neither side's statuses reach the other's home.
    let srv = TestServer::new(true);
    let [alice, bob, carol, ..] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    follow(&srv, &alice, &account_id(&srv, "carol"), "").await;
    let own = create(&srv, &alice, "alice own", "public").await;
    let _bob_post = create(&srv, &bob, "bob public", "public").await;
    let _carol_post = create(&srv, &carol, "carol public", "public").await;

    block(&srv, &alice, &account_id(&srv, "bob")).await; // viewer blocks
    block(&srv, &carol, &account_id(&srv, "alice")).await; // author blocks viewer

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(got, vec![own]);
}

#[tokio::test]
async fn home_show_reblogs_false_hides_that_follows_reblogs() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    let bob_id = account_id(&srv, "bob");
    let dave_id = account_id(&srv, "dave");
    follow(&srv, &alice, &bob_id, "reblogs=true").await;
    follow(&srv, &alice, &dave_id, "reblogs=false").await;

    let original = create(&srv, &carol, "carol original", "public").await;
    reblog(&srv, &bob, &original).await;
    reblog(&srv, &dave, &original).await;

    // Exactly one wrapper: bob's. Dave's was hidden by show_reblogs=false.
    let (status, _h, body) = get(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body[0]["reblog"]["id"], original.as_str());
    assert_eq!(body[0]["account"]["id"], bob_id.as_str());
}

#[tokio::test]
async fn home_reply_rules_follow_the_parent() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    follow(&srv, &alice, &account_id(&srv, "dave"), "").await;

    let carol_post = create(&srv, &carol, "carol post", "public").await;
    let dave_post = create(&srv, &dave, "dave post", "public").await;
    let alice_post = create(&srv, &alice, "alice post", "public").await;

    let reply_to_carol = create_with(
        &srv,
        &bob,
        &status_form("reply to carol", "public", &format!("in_reply_to_id={carol_post}")),
    )
    .await;
    let reply_to_dave = create_with(
        &srv,
        &bob,
        &status_form("reply to dave", "public", &format!("in_reply_to_id={dave_post}")),
    )
    .await;
    let reply_to_alice = create_with(
        &srv,
        &bob,
        &status_form("reply to alice", "public", &format!("in_reply_to_id={alice_post}")),
    )
    .await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert!(!got.contains(&reply_to_carol), "reply to a non-followed parent is out");
    assert!(got.contains(&reply_to_dave), "reply to a followed parent is in");
    assert!(got.contains(&reply_to_alice), "reply to the viewer is in");
}

#[tokio::test]
async fn home_own_statuses_always_appear_even_when_direct() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    let own_direct = create(&srv, &alice, "to myself nobody", "direct").await;
    let other = create(&srv, &bob, "not followed", "public").await;

    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert!(got.contains(&own_direct), "viewer's own statuses always show");
    assert!(!got.contains(&other));
}

// ----- public --------------------------------------------------------------

#[tokio::test]
async fn public_is_public_only_with_self_replies_and_no_reblogs() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    let a_public = create(&srv, &alice, "public one", "public").await;
    let _unlisted = create(&srv, &alice, "unlisted one", "unlisted").await;
    let _private = create(&srv, &alice, "private one", "private").await;
    let _direct = create(&srv, &alice, "direct one", "direct").await;
    let self_reply = create_with(
        &srv,
        &alice,
        &status_form("self reply", "public", &format!("in_reply_to_id={a_public}")),
    )
    .await;
    let other_reply = create_with(
        &srv,
        &bob,
        &status_form("reply to alice", "public", &format!("in_reply_to_id={a_public}")),
    )
    .await;
    reblog(&srv, &bob, &a_public).await;

    let got = ids_of(&srv, "/api/v1/timelines/public", None).await;
    assert_eq!(got, vec![self_reply, a_public], "public, self-replies only, no reblogs");
    assert!(!got.contains(&other_reply), "replies to others are dropped");
}

#[tokio::test]
async fn public_excludes_blocked_blocked_by_and_muted_authors_for_the_viewer() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, eve] = five_users(&srv).await;
    let bob_post = create(&srv, &bob, "bob public", "public").await;
    let carol_post = create(&srv, &carol, "carol public", "public").await;
    let dave_post = create(&srv, &dave, "dave public", "public").await;
    let eve_post = create(&srv, &eve, "eve public", "public").await;

    block(&srv, &alice, &account_id(&srv, "carol")).await; // viewer blocks
    block(&srv, &dave, &account_id(&srv, "alice")).await; // author blocks viewer
    mute(&srv, &alice, &account_id(&srv, "eve")).await; // viewer mutes

    let got = ids_of(&srv, "/api/v1/timelines/public", Some(&alice)).await;
    assert_eq!(got, vec![bob_post], "only bob's post survives: {got:?}");
    assert!(!got.contains(&carol_post));
    assert!(!got.contains(&dave_post));
    assert!(!got.contains(&eve_post));

    // Anonymous viewers are not subject to any viewer-side filtering.
    let anon = ids_of(&srv, "/api/v1/timelines/public", None).await;
    assert_eq!(anon.len(), 4, "{anon:?}");
}

#[tokio::test]
async fn public_local_flag_and_only_media_scope_the_feed() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    let a = create(&srv, &alice, "alice public", "public").await;
    let media = upload_png(&srv, &bob).await;
    let with_media = create_with(
        &srv,
        &bob,
        &status_form("bob picture", "public", &format!("media_ids[]={media}")),
    )
    .await;

    // Every status here is local, so `local=true` keeps all of them and
    // `remote=true` keeps none.
    let local = ids_of(&srv, "/api/v1/timelines/public?local=true", None).await;
    assert_eq!(local, vec![with_media.clone(), a.clone()]);
    let remote = ids_of(&srv, "/api/v1/timelines/public?remote=true", None).await;
    assert!(remote.is_empty(), "no remote statuses exist yet: {remote:?}");

    // `only_media` keeps just the status that carries an attachment.
    let media_only = ids_of(&srv, "/api/v1/timelines/public?only_media=true", None).await;
    assert_eq!(media_only, vec![with_media]);
}

#[tokio::test]
async fn domain_blocked_authors_are_excluded_for_the_viewer_only() {
    // Remote accounts cannot be created before federation (Phase 6), so the
    // domain rule is exercised through the local domain, which every local
    // account shares. The viewer's own statuses stay visible.
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    let own = create(&srv, &alice, "alice own", "public").await;
    let _from_bob = create(&srv, &bob, "bob public", "public").await;

    let (status, _h, body) = send_form(
        &srv,
        "POST",
        "/api/v1/domain_blocks",
        "domain=example.social",
        &alice,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let got = ids_of(&srv, "/api/v1/timelines/public", Some(&alice)).await;
    assert_eq!(got, vec![own.clone()], "blocked domain removed for the viewer");
    let anon = ids_of(&srv, "/api/v1/timelines/public", None).await;
    assert_eq!(anon.len(), 2, "anonymous viewers do not apply the viewer's blocks");
    let home = ids_of(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(home, vec![own], "followed bob is removed by the domain block");
}

async fn send_form(
    srv: &TestServer,
    method: &str,
    path: &str,
    body: &str,
    token: &str,
) -> (StatusCode, HeaderMap, String) {
    let (status, headers, bytes) =
        send(&srv.router, form_request(method, path, body, Some(token))).await;
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

// ----- tag -----------------------------------------------------------------

#[tokio::test]
async fn tag_timeline_normalizes_case_and_keeps_replies() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    let first = create(&srv, &alice, "#Rust is nice", "public").await;
    let second = create(&srv, &bob, "more #rust here", "public").await;
    let reply = create_with(
        &srv,
        &bob,
        &status_form("#rust reply", "public", &format!("in_reply_to_id={first}")),
    )
    .await;
    let unrelated = create(&srv, &bob, "no tag here", "public").await;

    // A reblog wrapper carries no hashtags of its own, so the hashtag index
    // never holds it: the boost of `first` stays out of the tag feed. (Open
    // question against the handoff's "reblogs present" note; see the log.)
    reblog(&srv, &bob, &first).await;
    let got = ids_of(&srv, "/api/v1/timelines/tag/rust", None).await;
    assert_eq!(got, vec![reply.clone(), second.clone(), first.clone()]);
    assert!(!got.contains(&unrelated));

    // Case and a leading `#` normalize to the same tag.
    assert_eq!(ids_of(&srv, "/api/v1/timelines/tag/RUST", None).await, got);
    assert_eq!(ids_of(&srv, "/api/v1/timelines/tag/%23rust", None).await, got);
}

#[tokio::test]
async fn tag_timeline_unknown_tag_is_an_empty_array_without_link() {
    let srv = TestServer::new(true);
    let (status, headers, body) = get(&srv, "/api/v1/timelines/tag/nothinghere", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
    assert!(header(&headers, "link").is_none(), "empty page has no Link");
    assert_eq!(header(&headers, "cache-control").as_deref(), Some(PUBLIC_CACHE));
}

// ----- list ----------------------------------------------------------------

/// Alice's list with bob and carol as members; alice follows dave too.
async fn list_fixture(srv: &TestServer, alice: &str) -> String {
    let created = post_ok(srv, "/api/v1/lists", alice, "title=Friends").await;
    let list_id = created["id"].as_str().unwrap().to_string();
    for name in ["bob", "carol", "dave"] {
        follow(srv, alice, &account_id(srv, name), "").await;
    }
    let members = format!(
        "account_ids[]={}&account_ids[]={}",
        account_id(srv, "bob"),
        account_id(srv, "carol"),
    );
    post_ok(srv, &format!("/api/v1/lists/{list_id}/accounts"), alice, &members).await;
    list_id
}

#[tokio::test]
async fn list_timeline_shows_member_statuses_only() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    let list_id = list_fixture(&srv, &alice).await;

    let b = create(&srv, &bob, "bob member", "public").await;
    let c = create(&srv, &carol, "carol member", "public").await;
    let d = create(&srv, &dave, "dave not a member", "public").await;

    let got = ids_of(&srv, &format!("/api/v1/timelines/list/{list_id}"), Some(&alice)).await;
    assert_eq!(got, vec![c, b]);
    assert!(!got.contains(&d));
}

#[tokio::test]
async fn list_replies_policy_list_keeps_replies_to_members() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    let list_id = list_fixture(&srv, &alice).await;
    let carol_post = create(&srv, &carol, "carol", "public").await;
    let dave_post = create(&srv, &dave, "dave", "public").await;
    let to_carol = create_with(
        &srv,
        &bob,
        &status_form("to carol", "public", &format!("in_reply_to_id={carol_post}")),
    )
    .await;
    let to_dave = create_with(
        &srv,
        &bob,
        &status_form("to dave", "public", &format!("in_reply_to_id={dave_post}")),
    )
    .await;

    // Default policy for lists is `list`.
    let got = ids_of(&srv, &format!("/api/v1/timelines/list/{list_id}"), Some(&alice)).await;
    assert!(got.contains(&to_carol), "reply to a member is kept: {got:?}");
    assert!(!got.contains(&to_dave), "reply to a non-member is dropped: {got:?}");
}

#[tokio::test]
async fn list_replies_policy_followed_and_none() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, dave, _eve] = five_users(&srv).await;
    let list_id = list_fixture(&srv, &alice).await;
    let carol_post = create(&srv, &carol, "carol", "public").await;
    let dave_post = create(&srv, &dave, "dave", "public").await;
    let to_dave = create_with(
        &srv,
        &bob,
        &status_form("to dave", "public", &format!("in_reply_to_id={dave_post}")),
    )
    .await;
    let to_carol = create_with(
        &srv,
        &bob,
        &status_form("to carol", "public", &format!("in_reply_to_id={carol_post}")),
    )
    .await;
    let list_url = format!("/api/v1/timelines/list/{list_id}");

    // `followed`: alice follows dave (not a member) so the reply to dave is kept.
    put_ok(&srv, &format!("/api/v1/lists/{list_id}"), &alice, "replies_policy=followed").await;
    let followed = ids_of(&srv, &list_url, Some(&alice)).await;
    assert!(followed.contains(&to_dave), "followed policy keeps replies to followed: {followed:?}");
    assert!(followed.contains(&to_carol));

    // `none`: only replies to the viewer or to the author's own thread.
    put_ok(&srv, &format!("/api/v1/lists/{list_id}"), &alice, "replies_policy=none").await;
    let none = ids_of(&srv, &list_url, Some(&alice)).await;
    assert!(!none.contains(&to_dave), "none drops replies to others: {none:?}");
    assert!(!none.contains(&to_carol), "none drops replies to members too: {none:?}");
}

#[tokio::test]
async fn list_errors_are_exact() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    let list_id = list_fixture(&srv, &alice).await;
    let url = format!("/api/v1/timelines/list/{list_id}");

    // Not owned: exact Mastodon 404 body.
    let (status, _h, body) = get(&srv, &url, Some(&bob)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "Record not found" }));

    // Unparsable id is the same 404.
    let (status, _h, body) = get(&srv, "/api/v1/timelines/list/nope", Some(&alice)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "Record not found" }));

    // Application token has no user: 422.
    let app = register_app(&srv, "read write").await;
    let app_tok = app_token(&srv, &app, "read").await;
    let (status, _h, _b) = get(&srv, &url, Some(&app_tok)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Missing token: 401.
    let (status, _h, _b) = get(&srv, &url, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn home_requires_a_user_token() {
    let srv = TestServer::new(true);
    let (status, _h, _b) = get(&srv, "/api/v1/timelines/home", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let app = register_app(&srv, "read write").await;
    let app_tok = app_token(&srv, &app, "read").await;
    let (status, _h, _b) = get(&srv, "/api/v1/timelines/home", Some(&app_tok)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ----- headers and query contract -------------------------------------------

#[tokio::test]
async fn cache_control_follows_the_feed_and_the_viewer() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    create(&srv, &bob, "#tagged", "public").await;
    let list_id = list_fixture(&srv, &alice).await;

    let (_s, h, _b) = get(&srv, "/api/v1/timelines/public", None).await;
    assert_eq!(header(&h, "cache-control").as_deref(), Some(PUBLIC_CACHE));

    let (_s, h, _b) = get(&srv, "/api/v1/timelines/tag/tagged", None).await;
    assert_eq!(header(&h, "cache-control").as_deref(), Some(PUBLIC_CACHE));

    // Authenticated responses carry viewer-specific data, so never cache them.
    let (_s, h, _b) = get(&srv, "/api/v1/timelines/public", Some(&alice)).await;
    assert_eq!(header(&h, "cache-control").as_deref(), Some(PRIVATE_CACHE));

    let (_s, h, _b) = get(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(header(&h, "cache-control").as_deref(), Some(PRIVATE_CACHE));

    let (_s, h, _b) = get(&srv, &format!("/api/v1/timelines/list/{list_id}"), Some(&alice)).await;
    assert_eq!(header(&h, "cache-control").as_deref(), Some(PRIVATE_CACHE));
}

#[tokio::test]
async fn cursors_limit_and_link_headers_match_the_contract() {
    let srv = TestServer::new(true);
    let [alice, ..] = five_users(&srv).await;
    // s1..s45, oldest first.
    let mut s: Vec<String> = Vec::new();
    for n in 1..=45 {
        s.push(create(&srv, &alice, &format!("status {n}"), "public").await);
    }
    let base = "/api/v1/timelines/public";

    // Newest page: ids descending, Link next = oldest on page, prev = newest.
    let (status, h, body) = get(&srv, &format!("{base}?limit=2"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), vec![s[44].clone(), s[43].clone()]);
    assert_eq!(
        header(&h, "link").unwrap(),
        format!(
            "<{base}?limit=2&max_id={}>; rel=\"next\", <{base}?limit=2&min_id={}>; rel=\"prev\"",
            s[43], s[44]
        )
    );

    // max_id is exclusive.
    let got = ids_of(&srv, &format!("{base}?limit=2&max_id={}", s[43]), None).await;
    assert_eq!(got, vec![s[42].clone(), s[41].clone()]);

    // since_id is exclusive and returns the newest `limit` above it.
    let got = ids_of(&srv, &format!("{base}?limit=2&since_id={}", s[39]), None).await;
    assert_eq!(got, vec![s[44].clone(), s[43].clone()]);

    // min_id pages ascending from the bound, then re-sorted newest first.
    let (_s, h, body) = get(&srv, &format!("{base}?limit=2&min_id={}", s[39]), None).await;
    assert_eq!(ids(&body), vec![s[41].clone(), s[40].clone()]);
    assert_eq!(
        header(&h, "link").unwrap(),
        format!(
            "<{base}?limit=2&max_id={}>; rel=\"next\", <{base}?limit=2&min_id={}>; rel=\"prev\"",
            s[40], s[41]
        )
    );

    // Empty page: no Link header.
    let (status, h, body) = get(&srv, &format!("{base}?max_id={}", s[0]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
    assert!(header(&h, "link").is_none());

    // limit=0 and non-numeric limit are empty pages (Mastodon limit_param).
    let (_s, h, body) = get(&srv, &format!("{base}?limit=0"), None).await;
    assert_eq!(body, serde_json::json!([]));
    assert!(header(&h, "link").is_none());
    let (_s, _h, body) = get(&srv, &format!("{base}?limit=abc"), None).await;
    assert_eq!(body, serde_json::json!([]));

    // limit above the ceiling is clamped to 40.
    let got = ids_of(&srv, &format!("{base}?limit=999"), None).await;
    assert_eq!(got.len(), 40);
    assert_eq!(got.first(), Some(&s[44]));

    // Non-numeric cursor is a 400.
    let (status, _h, body) = get(&srv, &format!("{base}?max_id=abc"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({ "error": "Invalid cursor value" }));
}

// ----- sparse filter and bounded scans ----------------------------------------

#[tokio::test]
async fn sparse_filter_scan_stays_bounded() {
    let srv = TestServer::new(true);
    let [alice, bob, carol, ..] = five_users(&srv).await;
    // 280 statuses from bob (muted by alice) and 20 from carol, interleaved so
    // the scan has to walk past every muted entry to fill the page.
    for n in 0..300 {
        if n % 15 == 14 {
            create(&srv, &carol, &format!("carol {n}"), "public").await;
        } else {
            create(&srv, &bob, &format!("bob {n}"), "public").await;
        }
    }
    mute(&srv, &alice, &account_id(&srv, "bob")).await;

    let before = srv.store.metrics();
    let (status, h, body) = get(&srv, "/api/v1/timelines/public?limit=20", Some(&alice)).await;
    let after = srv.store.metrics();
    assert_eq!(status, StatusCode::OK);
    let page = body.as_array().unwrap();
    assert_eq!(page.len(), 20, "filled to the limit from the sparse author");
    assert!(page.iter().all(|s| s["account"]["username"] == "carol"));
    assert!(header(&h, "link").is_some());

    let items_scanned = after.items_scanned - before.items_scanned;
    let read_txns = after.read_txns - before.read_txns;
    // Measured (see docs/logs/2026-10-17-phase-4-verification.md): the 20th
    // carol status is the 286th index entry from the newest, and the request
    // needs 13 read transactions: one scan, two lookups per distinct author
    // seen (bob, carol), and the constant-size render loads.
    eprintln!("sparse_filter_scan_stays_bounded: items_scanned={items_scanned} read_txns={read_txns}");
    assert!(items_scanned <= 5_000, "items_scanned={items_scanned}");
    assert!(read_txns <= 600, "read_txns={read_txns}");
}

// ----- filtered annotation ---------------------------------------------------

#[tokio::test]
async fn home_timeline_carries_the_filtered_annotation() {
    let srv = TestServer::new(true);
    let [alice, bob, ..] = five_users(&srv).await;
    follow(&srv, &alice, &account_id(&srv, "bob"), "").await;
    post_ok(
        &srv,
        "/api/v2/filters",
        &alice,
        "title=Noise&context[]=home&filter_action=warn&keywords_attributes[0][keyword]=spoiler",
    )
    .await;

    let matching = create(&srv, &bob, "a spoiler inside", "public").await;
    let plain = create(&srv, &bob, "nothing to see", "public").await;

    let (status, _h, body) = get(&srv, "/api/v1/timelines/home", Some(&alice)).await;
    assert_eq!(status, StatusCode::OK);
    let find = |id: &str| body.as_array().unwrap().iter().find(|s| s["id"] == id).unwrap().clone();
    let hit = find(&matching);
    assert!(
        hit["filtered"].as_array().is_some_and(|a| !a.is_empty()),
        "matching status carries filter results: {hit}"
    );
    assert_eq!(find(&plain)["filtered"], serde_json::json!([]));
}
