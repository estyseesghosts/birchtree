//! Phase 4.1 (D10b): members of the viewer's exclusive lists are left out of
//! home, as upstream's `FeedManager#filter_from_home` does (`:skip_home`).
//!
//! The three rules: the viewer's own statuses are never left out (upstream
//! returns before the check); list timelines are unaffected (the rule applies
//! only when the timeline type is not `list`); and only lists with
//! `exclusive=true` count.

mod common;

use axum::http::StatusCode;
use serde_json::Value;

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

/// A user token with the scopes home, lists and statuses need.
async fn user_token(srv: &TestServer, username: &str) -> String {
    let body = format!(
        "client_name=ExclusiveHome&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read write"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let token_body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope=write",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &token_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_token = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let account_body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &account_body, Some(&app_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

fn account_id(srv: &TestServer, username: &str) -> String {
    use birchtree::application::ports::AccountStore;
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

/// POST a form as `token`, asserting 200, and return the body.
async fn post_ok(srv: &TestServer, token: &str, path: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn put_ok(srv: &TestServer, token: &str, path: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("PUT", path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "PUT {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn post_status(srv: &TestServer, token: &str, text: &str) -> String {
    let body = format!("status={}&visibility=public", enc(text));
    post_ok(srv, token, "/api/v1/statuses", &body).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn follow(srv: &TestServer, token: &str, target: &str) {
    post_ok(srv, token, &format!("/api/v1/accounts/{target}/follow"), "").await;
}

async fn create_list(srv: &TestServer, token: &str, title: &str, exclusive: bool) -> String {
    let body = format!("title={}&exclusive={exclusive}", enc(title));
    post_ok(srv, token, "/api/v1/lists", &body).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn add_member(srv: &TestServer, token: &str, list: &str, account: &str) {
    post_ok(
        srv,
        token,
        &format!("/api/v1/lists/{list}/accounts"),
        &format!("account_ids[]={account}"),
    )
    .await;
}

async fn set_exclusive(srv: &TestServer, token: &str, list: &str, exclusive: bool) {
    put_ok(srv, token, &format!("/api/v1/lists/{list}"), &format!("exclusive={exclusive}")).await;
}

/// Status ids of a timeline, in response order.
async fn timeline_ids(srv: &TestServer, token: &str, uri: &str) -> Vec<String> {
    let (status, _h, bytes) = send(&srv.router, get_request(uri, Some(token), None)).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|status| status["id"].as_str().unwrap().to_string())
        .collect()
}

/// Alice follows Bob and Carol. Bob and Carol each post once.
struct Scene {
    srv: TestServer,
    alice: String,
    bob: String,
    bob_id: String,
    carol_id: String,
    bob_post: String,
    carol_post: String,
}

async fn scene() -> Scene {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;
    let carol = user_token(&srv, "carol").await;
    let bob_id = account_id(&srv, "bob");
    let carol_id = account_id(&srv, "carol");
    follow(&srv, &alice, &bob_id).await;
    follow(&srv, &alice, &carol_id).await;
    let bob_post = post_status(&srv, &bob, "bob says hi").await;
    let carol_post = post_status(&srv, &carol, "carol says hi").await;
    Scene { srv, alice, bob, bob_id, carol_id, bob_post, carol_post }
}

#[tokio::test]
async fn exclusive_member_is_left_out_of_home_and_shown_on_its_list() {
    let s = scene().await;
    let vip = create_list(&s.srv, &s.alice, "VIP", true).await;
    add_member(&s.srv, &s.alice, &vip, &s.bob_id).await;

    let home = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(!home.contains(&s.bob_post), "exclusive member must be hidden from home: {home:?}");
    assert!(home.contains(&s.carol_post), "non-member still shows on home: {home:?}");

    // List timelines do not apply the rule.
    let on_list = timeline_ids(&s.srv, &s.alice, &format!("/api/v1/timelines/list/{vip}")).await;
    assert_eq!(on_list, vec![s.bob_post.clone()], "member shows on its own list");
}

#[tokio::test]
async fn viewers_own_statuses_and_reblogs_are_never_left_out() {
    let s = scene().await;
    let vip = create_list(&s.srv, &s.alice, "VIP", true).await;
    add_member(&s.srv, &s.alice, &vip, &s.bob_id).await;
    let own = post_status(&s.srv, &s.alice, "alice says hi").await;
    // Alice reblogs Bob's post. The wrapper is Alice's status, so it is shown;
    // Bob's original is not.
    let reblog = post_ok(&s.srv, &s.alice, &format!("/api/v1/statuses/{}/reblog", s.bob_post), "").await;
    let reblog_id = reblog["id"].as_str().unwrap().to_string();

    let home = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(home.contains(&own), "own status shows: {home:?}");
    assert!(home.contains(&reblog_id), "own reblog shows: {home:?}");
    assert!(!home.contains(&s.bob_post), "bob's original is hidden: {home:?}");
}

#[tokio::test]
async fn non_exclusive_list_hides_nothing() {
    let s = scene().await;
    let chat = create_list(&s.srv, &s.alice, "Chat", false).await;
    add_member(&s.srv, &s.alice, &chat, &s.bob_id).await;

    let home = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(home.contains(&s.bob_post), "a non-exclusive member stays on home: {home:?}");
    assert!(home.contains(&s.carol_post), "{home:?}");
}

#[tokio::test]
async fn clearing_exclusive_restores_the_member_on_home() {
    let s = scene().await;
    let vip = create_list(&s.srv, &s.alice, "VIP", true).await;
    add_member(&s.srv, &s.alice, &vip, &s.bob_id).await;
    let hidden = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(!hidden.contains(&s.bob_post));

    set_exclusive(&s.srv, &s.alice, &vip, false).await;
    let shown = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(shown.contains(&s.bob_post), "unset exclusive: member is back: {shown:?}");
}

#[tokio::test]
async fn only_the_viewers_own_exclusive_lists_count() {
    let s = scene().await;
    // Bob puts Carol on his exclusive list. Alice's home is unaffected.
    let bobs = create_list(&s.srv, &s.bob, "Bob's VIP", true).await;
    follow(&s.srv, &s.bob, &s.carol_id).await;
    add_member(&s.srv, &s.bob, &bobs, &s.carol_id).await;

    let home = timeline_ids(&s.srv, &s.alice, "/api/v1/timelines/home").await;
    assert!(home.contains(&s.carol_post), "another viewer's exclusive list hides nothing: {home:?}");
}
