//! Phase 3 relationship contract tests: follows and follow requests,
//! blocks, mutes, domain blocks, private account notes and endorsements —
//! every write answers 200 with a fresh `REST::RelationshipSerializer`.

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

/// Register an account and return `(user_token, token_body)`.
async fn register_account(
    srv: &TestServer,
    app: &serde_json::Value,
    username: &str,
) -> (String, serde_json::Value) {
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    (body["access_token"].as_str().unwrap().to_string(), body)
}

const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts \
read:follows write:follows read:statuses write:statuses read:favourites \
write:favourites read:bookmarks write:bookmarks read:blocks write:blocks \
read:mutes write:mutes";

/// The account id behind a user token (`verify_credentials`).
async fn account_id(srv: &TestServer, token: &str) -> String {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

/// The full credential document (carries `source.follow_requests_count`).
async fn credentials(srv: &TestServer, token: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// The public account document (carries `followers_count`/`following_count`).
async fn account_show(srv: &TestServer, id: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{id}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// The viewer's relationship to `target` via `GET .../relationships`.
async fn relationship(srv: &TestServer, token: &str, target: &str) -> Value {
    let uri = format!("/api/v1/accounts/relationships?id[]={target}");
    let (status, _h, bytes) = send(&srv.router, get_request(&uri, Some(token), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let all = json(&bytes);
    assert!(all.is_array(), "{all}");
    all[0].clone()
}

/// `POST /api/v1/accounts/{target}/follow` with a form body (asserts 200).
async fn follow(srv: &TestServer, token: &str, target: &str, form: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target}/follow"),
            form,
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// `POST /api/v1/accounts/{target}/unfollow` (asserts 200).
async fn unfollow(srv: &TestServer, token: &str, target: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target}/unfollow"),
            "",
            Some(token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// `GET /api/v1/endorsements` for the viewer (asserts 200, returns the array).
async fn endorsements(srv: &TestServer, token: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/endorsements", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

#[tokio::test]
async fn relationship_dto_contract_shape() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _body) = register_account(&srv, &app, "relviewer").await;
    let other = common::mk_account(&srv.store, "relother");

    let rel = relationship(&srv, &token, &other.id.to_string()).await;

    // Exact `REST::RelationshipSerializer` key set — no more, no less.
    let mut keys: Vec<String> = rel.as_object().expect("relationship object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    let expected = [
        "blocked_by",
        "blocking",
        "domain_blocking",
        "endorsed",
        "followed_by",
        "following",
        "id",
        "languages",
        "muting",
        "muting_expires_at",
        "muting_notifications",
        "note",
        "notifying",
        "requested",
        "requested_by",
        "showing_reblogs",
    ];
    assert_eq!(keys, expected);

    // Neutral pair: every flag false, `showing_reblogs` false, empty note,
    // `languages` and `muting_expires_at` null.
    assert_eq!(rel["id"], other.id.to_string());
    for flag in [
        "following",
        "showing_reblogs",
        "notifying",
        "followed_by",
        "blocking",
        "blocked_by",
        "muting",
        "muting_notifications",
        "requested",
        "requested_by",
        "domain_blocking",
        "endorsed",
    ] {
        assert_eq!(rel[flag], false, "{flag}");
    }
    assert_eq!(rel["note"], "");
    assert_eq!(rel["languages"], Value::Null);
    assert_eq!(rel["muting_expires_at"], Value::Null);
}

#[tokio::test]
async fn follow_updates_relationship_and_stats() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "followalice").await;
    let (bob, _b) = register_account(&srv, &app, "followbob").await;
    let bob_id = account_id(&srv, &bob).await;

    // Fresh follow: active, reblogs shown by default, no notification.
    let rel = follow(&srv, &alice, &bob_id, "").await;
    assert_eq!(rel["following"], true);
    assert_eq!(rel["showing_reblogs"], true);
    assert_eq!(rel["notifying"], false);
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 1);
    let alice_id = account_id(&srv, &alice).await;
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 1);

    // Re-follow with `reblogs=false` flips the flag without double-counting.
    let rel = follow(&srv, &alice, &bob_id, "reblogs=false").await;
    assert_eq!(rel["following"], true);
    assert_eq!(rel["showing_reblogs"], false);
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 1);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 1);

    // `notify=true` lights up `notifying`.
    let rel = follow(&srv, &alice, &bob_id, "reblogs=false&notify=true").await;
    assert_eq!(rel["following"], true);
    assert_eq!(rel["showing_reblogs"], false);
    assert_eq!(rel["notifying"], true);
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 1);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 1);
}

#[tokio::test]
async fn unfollow_and_remove_from_followers_revert_counters() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "unfalice").await;
    let (bob, _b) = register_account(&srv, &app, "unfbob").await;
    let (carol, _b) = register_account(&srv, &app, "unfcarol").await;
    let alice_id = account_id(&srv, &alice).await;
    let bob_id = account_id(&srv, &bob).await;
    let carol_id = account_id(&srv, &carol).await;

    // Unfollow reverts the relationship and both counters.
    follow(&srv, &alice, &bob_id, "").await;
    let rel = unfollow(&srv, &alice, &bob_id).await;
    assert_eq!(rel["following"], false);
    assert_eq!(rel["showing_reblogs"], false);
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 0);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 0);

    // Carol follows Alice; Alice never follows back. Alice removes Carol.
    follow(&srv, &carol, &alice_id, "").await;
    assert_eq!(account_show(&srv, &alice_id).await["followers_count"], 1);
    assert_eq!(account_show(&srv, &carol_id).await["following_count"], 1);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{carol_id}/remove_from_followers"),
            "",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["id"], carol_id);
    assert_eq!(rel["following"], false);
    assert_eq!(rel["followed_by"], false);
    assert_eq!(account_show(&srv, &carol_id).await["following_count"], 0);
    assert_eq!(account_show(&srv, &alice_id).await["followers_count"], 0);

    // Removing someone who never followed you is a 200 with a neutral shape.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{bob_id}/remove_from_followers"),
            "",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["following"], false);
    assert_eq!(rel["followed_by"], false);
    assert_eq!(rel["blocking"], false);
    assert_eq!(rel["note"], "");
}

#[tokio::test]
async fn self_follow_and_unknown_target_errors() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _b) = register_account(&srv, &app, "selfy").await;
    let me = account_id(&srv, &token).await;

    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{me}/follow"), "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        json(&bytes)["error"],
        "Following your own account is not allowed"
    );

    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/accounts/999999999/follow",
            "",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "Record not found");
}

#[tokio::test]
async fn locked_account_follow_request_lifecycle() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (target, _b) = register_account(&srv, &app, "locktarget").await;
    let (req1, _b) = register_account(&srv, &app, "lockreq1").await;
    let (req2, _b) = register_account(&srv, &app, "lockreq2").await;
    let target_id = account_id(&srv, &target).await;
    let req1_id = account_id(&srv, &req1).await;
    let req2_id = account_id(&srv, &req2).await;

    // The target locks its account.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "locked=true",
            Some(&target),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["locked"], true);

    // Following a locked account opens a request, not a follow.
    let rel = follow(&srv, &req1, &target_id, "").await;
    assert_eq!(rel["requested"], true);
    assert_eq!(rel["following"], false);

    // A duplicate follow keeps the request pending and does not inflate
    // counters: no followers yet, exactly one pending request.
    let rel = follow(&srv, &req1, &target_id, "").await;
    assert_eq!(rel["requested"], true);
    assert_eq!(rel["following"], false);
    assert_eq!(account_show(&srv, &target_id).await["followers_count"], 0);
    let creds = credentials(&srv, &target).await;
    assert_eq!(creds["source"]["follow_requests_count"], 1);

    // The request list carries the requester as an `AccountSerializer`.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/follow_requests", Some(&target), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let requests = json(&bytes);
    assert_eq!(requests.as_array().unwrap().len(), 1);
    assert_eq!(requests[0]["username"], "lockreq1");
    assert_eq!(requests[0]["acct"], "lockreq1");
    assert_eq!(requests[0]["id"], req1_id);

    // Authorize answers with the RELATIONSHIP OF THE TARGET to the requester:
    // the requester now follows the target (`followed_by` from the target's
    // perspective).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/follow_requests/{req1_id}/authorize"),
            "",
            Some(&target),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["id"], req1_id);
    assert_eq!(rel["followed_by"], true);
    assert_eq!(rel["requested"], false);
    let req1_rel = relationship(&srv, &req1, &target_id).await;
    assert_eq!(req1_rel["following"], true);
    assert_eq!(req1_rel["requested"], false);
    assert_eq!(account_show(&srv, &target_id).await["followers_count"], 1);
    assert_eq!(credentials(&srv, &target).await["source"]["follow_requests_count"], 0);

    // A second requester opens a new request; rejecting drops it everywhere.
    follow(&srv, &req2, &target_id, "").await;
    assert_eq!(credentials(&srv, &target).await["source"]["follow_requests_count"], 1);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/follow_requests/{req2_id}/reject"),
            "",
            Some(&target),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["id"], req2_id);
    assert_eq!(rel["requested"], false);
    let req2_rel = relationship(&srv, &req2, &target_id).await;
    assert_eq!(req2_rel["following"], false);
    assert_eq!(req2_rel["requested"], false);
    let target_rel = relationship(&srv, &target, &req2_id).await;
    assert_eq!(target_rel["followed_by"], false);
    assert_eq!(credentials(&srv, &target).await["source"]["follow_requests_count"], 0);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/follow_requests", Some(&target), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn block_removes_follows_and_endorsements_both_ways() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "blkalice").await;
    let (bob, _b) = register_account(&srv, &app, "blkbob").await;
    let alice_id = account_id(&srv, &alice).await;
    let bob_id = account_id(&srv, &bob).await;

    // Mutual follows and mutual endorsements.
    follow(&srv, &alice, &bob_id, "").await;
    follow(&srv, &bob, &alice_id, "").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{alice_id}/pin"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["endorsed"], true);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/pin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["endorsed"], true);
    assert_eq!(endorsements(&srv, &alice).await.as_array().unwrap().len(), 1);
    assert_eq!(endorsements(&srv, &bob).await.as_array().unwrap().len(), 1);

    // Alice blocks Bob: both follows, both endorsements and both counters go.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/block"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["blocking"], true);
    assert_eq!(rel["following"], false);

    let alice_view = relationship(&srv, &alice, &bob_id).await;
    assert_eq!(alice_view["blocking"], true);
    assert_eq!(alice_view["following"], false);
    assert_eq!(alice_view["endorsed"], false);
    let bob_view = relationship(&srv, &bob, &alice_id).await;
    assert_eq!(bob_view["followed_by"], false);
    assert_eq!(bob_view["blocking"], false);
    assert_eq!(bob_view["blocked_by"], true);
    assert_eq!(bob_view["endorsed"], false);
    assert_eq!(endorsements(&srv, &alice).await.as_array().unwrap().len(), 0);
    assert_eq!(endorsements(&srv, &bob).await.as_array().unwrap().len(), 0);
    assert_eq!(account_show(&srv, &alice_id).await["followers_count"], 0);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 0);
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 0);
    assert_eq!(account_show(&srv, &bob_id).await["following_count"], 0);

    // Unblock clears only the block; the severed follows stay gone.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/unblock"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["blocking"], false);
    assert_eq!(relationship(&srv, &alice, &bob_id).await["following"], false);
    assert_eq!(relationship(&srv, &bob, &alice_id).await["followed_by"], false);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 0);
    assert_eq!(account_show(&srv, &bob_id).await["following_count"], 0);
}

#[tokio::test]
async fn blocked_pair_cannot_follow() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "nofalice").await;
    let (bob, _b) = register_account(&srv, &app, "nofbob").await;
    let alice_id = account_id(&srv, &alice).await;
    let bob_id = account_id(&srv, &bob).await;

    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/block"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // Either direction of the blocked pair refuses the follow.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{alice_id}/follow"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "This action is not allowed");

    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/follow"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "This action is not allowed");
}

#[tokio::test]
async fn mute_duration_and_notifications() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "mutalice").await;
    let (bob, _b) = register_account(&srv, &app, "mutbob").await;
    let bob_id = account_id(&srv, &bob).await;

    // Permanent mute, absent `notifications` param: notifications hidden.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/mute"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["muting"], true);
    assert_eq!(rel["muting_expires_at"], Value::Null);
    assert_eq!(rel["muting_notifications"], true);

    // `notifications=true` keeps notifications visible.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{bob_id}/mute"),
            "notifications=true",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["muting"], true);
    assert_eq!(rel["muting_notifications"], false);

    // A timed mute carries an RFC3339 expiry in the future (~now + 3600s).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{bob_id}/mute"),
            "duration=3600",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["muting"], true);
    assert_eq!(rel["muting_notifications"], true);
    let expires = rel["muting_expires_at"].as_str().expect("expiry string");
    let parsed = chrono::DateTime::parse_from_rfc3339(expires)
        .unwrap_or_else(|err| panic!("muting_expires_at not RFC3339: {expires}: {err}"));
    let now = chrono::Utc::now();
    assert!(parsed > now, "expiry {expires} must be in the future");
    let drift = (parsed.timestamp() - now.timestamp()).abs();
    assert!(drift <= 3600, "expiry ~1h away, drift {drift}s for {expires}");

    // Unmute clears the mute and its expiry.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/unmute"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let rel = json(&bytes);
    assert_eq!(rel["muting"], false);
    assert_eq!(rel["muting_expires_at"], Value::Null);
    assert_eq!(rel["muting_notifications"], false);
}

#[tokio::test]
async fn account_note_lifecycle() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "notealice").await;
    let (bob, _b) = register_account(&srv, &app, "notebob").await;
    let target = common::mk_account(&srv.store, "notetarget");
    let target_id = target.id.to_string();

    // Set, then replace.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target_id}/note"),
            "comment=hello",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["note"], "hello");
    let body = format!("comment={}", enc("world signs"));
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{target_id}/note"), &body, Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["note"], "world signs");

    // Exactly 2000 characters is fine; 2001 fails with the pinned message.
    let ok_comment = "a".repeat(2000);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target_id}/note"),
            &format!("comment={ok_comment}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["note"].as_str().unwrap().chars().count(), 2000);
    let too_long = "a".repeat(2001);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target_id}/note"),
            &format!("comment={too_long}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Comment is too long (maximum is 2000 characters)"
    );

    // A blank comment deletes the note.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target_id}/note"),
            "comment=",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["note"], "");

    // The note is private to its author: another viewer sees "".
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{target_id}/note"),
            "comment=mine",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["note"], "mine");
    assert_eq!(relationship(&srv, &alice, &target_id).await["note"], "mine");
    assert_eq!(relationship(&srv, &bob, &target_id).await["note"], "");
}

#[tokio::test]
async fn endorsement_requires_follow_and_lists_work() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "endalice").await;
    let (bob, _b) = register_account(&srv, &app, "endbob").await;
    let alice_id = account_id(&srv, &alice).await;
    let bob_id = account_id(&srv, &bob).await;

    // Endorsing without an active follow is a validation error.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/pin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: You must be already following the person you want to endorse"
    );

    // Following first makes the endorsement stick; duplicates are idempotent.
    follow(&srv, &alice, &bob_id, "").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/pin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["endorsed"], true);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/pin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["endorsed"], true);

    // Both list endpoints carry the endorsed account.
    let list = endorsements(&srv, &alice).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["username"], "endbob");
    assert_eq!(list[0]["acct"], "endbob");
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{alice_id}/endorsements"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let list = json(&bytes);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["username"], "endbob");

    // Unendorse withdraws it.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/unpin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["endorsed"], false);
    assert_eq!(endorsements(&srv, &alice).await.as_array().unwrap().len(), 0);

    // Unfollowing auto-drops a standing endorsement.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/pin"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["endorsed"], true);
    unfollow(&srv, &alice, &bob_id).await;
    assert_eq!(relationship(&srv, &alice, &bob_id).await["endorsed"], false);
    assert_eq!(endorsements(&srv, &alice).await.as_array().unwrap().len(), 0);
}

/// The value of `key` in the `rel` entry of a `Link` header (`None` when the
/// relation is absent).
fn link_cursor(link: &str, rel: &str, key: &str) -> Option<String> {
    let entry = link
        .split(", ")
        .find(|part| part.ends_with(&format!("rel=\"{rel}\"")))?;
    let url = entry.split(';').next()?.trim().trim_start_matches('<').trim_end_matches('>');
    let (_, query) = url.split_once('?')?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{key}=")).map(str::to_string))
}

#[tokio::test]
async fn domain_blocks_crud_and_relationship_flag() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "domalice").await;
    let (bob, _b) = register_account(&srv, &app, "dombob").await;
    let target = common::mk_account(&srv.store, "domtarget");
    let target_id = target.id.to_string();

    // POST (upstream shape) answers `200 {}`; the list is newest first.
    for domain in ["zeta.social", "example.social"] {
        let (status, _h, bytes) = send(
            &srv.router,
            form_request("POST", "/api/v1/domain_blocks", &format!("domain={domain}"), Some(&alice)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        assert_eq!(json(&bytes), serde_json::json!({}));
    }
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/domain_blocks", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!(["example.social", "zeta.social"]));

    // The relationship flag reads the TARGET's domain; local accounts live on
    // the server domain (`example.social` here), so any local target flips it.
    assert_eq!(relationship(&srv, &alice, &target_id).await["domain_blocking"], true);
    // Bob did not block the domain: his flag stays false.
    assert_eq!(relationship(&srv, &bob, &target_id).await["domain_blocking"], false);

    // DELETE takes the domain as a query parameter and answers `200 {}`.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", "/api/v1/domain_blocks?domain=example.social", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!({}));
    assert_eq!(relationship(&srv, &alice, &target_id).await["domain_blocking"], false);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/domain_blocks", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!(["zeta.social"]));

    // DELETE is idempotent; the domain may also come from a form body.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", "/api/v1/domain_blocks?domain=example.social", "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", "/api/v1/domain_blocks", "domain=zeta.social", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (_s, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/domain_blocks", Some(&alice), None),
    )
    .await;
    assert_eq!(json(&bytes), serde_json::json!([]));

    // Blank or missing domains are 422 with upstream's RecordInvalid message.
    for body in ["domain=", ""] {
        let (status, _h, bytes) = send(
            &srv.router,
            form_request("POST", "/api/v1/domain_blocks", body, Some(&alice)),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(json(&bytes)["error"], "Validation failed: Domain can't be blank");
    }
    // Malformed domains are 422 "Domain is invalid".
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/domain_blocks",
            "domain=https%3A%2F%2Fnope.example%2Fpath",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(json(&bytes)["error"], "Validation failed: Domain is invalid");

    // The old shape is gone: PUT and DELETE-by-path are not routed.
    let (status, _h, _b) = send(
        &srv.router,
        form_request("PUT", "/api/v1/domain_blocks", "domain=gone.example", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn domain_blocks_page_by_row_id_like_upstream() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "pagealice").await;
    for domain in ["a.one", "b.two", "c.three"] {
        let (status, _h, bytes) = send(
            &srv.router,
            form_request("POST", "/api/v1/domain_blocks", &format!("domain={domain}"), Some(&alice)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    }

    // A full page of two: newest first, `next` = the oldest id on the page.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/domain_blocks?limit=2", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!(["c.three", "b.two"]));
    let link = headers
        .get("link")
        .expect("full page has a Link header")
        .to_str()
        .unwrap()
        .to_string();
    let next = link_cursor(&link, "next", "max_id").expect("next carries max_id");
    assert!(link_cursor(&link, "prev", "since_id").is_some(), "prev carries since_id");
    assert!(link_cursor(&link, "prev", "min_id").is_none(), "no min_id on this endpoint");

    // max_id is exclusive: the rest of the list.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/domain_blocks?limit=2&max_id={next}"), Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!(["a.one"]));
    assert!(headers.get("link").is_some_and(|h| {
        !h.to_str().unwrap().contains("rel=\"next\"")
    }), "a short page has no next link");

    // since_id is exclusive: blocks strictly newer than the oldest id on page one.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/domain_blocks?since_id={next}"), Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!(["c.three"]));
}

#[tokio::test]
async fn relationship_scopes_and_auth_errors() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _b) = register_account(&srv, &app, "scopey").await;
    let target = common::mk_account(&srv.store, "scopetarget");
    let target_id = target.id.to_string();
    let follow_uri = format!("/api/v1/accounts/{target_id}/follow");

    // A read-only token is out of scope.
    let read_token = app_token(&srv, &app, "read").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &follow_uri, "", Some(&read_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "This action is outside the authorized scopes");

    // An app-only token (client credentials) needs a user.
    let follow_token = app_token(&srv, &app, "follow").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &follow_uri, "", Some(&follow_token)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(json(&bytes)["error"], "This method requires an authenticated user");

    // No token at all is unauthorized.
    let (status, _h, bytes) = send(&srv.router, form_request("POST", &follow_uri, "", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "The access token is invalid");

    // A user token carrying `write`/`follow` scope passes (control case).
    let rel = follow(&srv, &token, &target_id, "").await;
    assert_eq!(rel["following"], true);
}

#[tokio::test]
async fn follow_unfollow_race_has_consistent_counters() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "racealice").await;
    let (bob, _b) = register_account(&srv, &app, "racebob").await;
    let alice_id = account_id(&srv, &alice).await;
    let bob_id = account_id(&srv, &bob).await;

    // 8 concurrent follows for the same pair must land exactly one active
    // follow (redb serializes the write transactions).
    let router = srv.router.clone();
    let follow_uri = format!("/api/v1/accounts/{bob_id}/follow");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = router.clone();
        let uri = follow_uri.clone();
        let token = alice.clone();
        handles.push(tokio::spawn(async move {
            send(&router, form_request("POST", &uri, "", Some(&token))).await
        }));
    }
    for handle in handles {
        let (status, _h, bytes) = handle.await.expect("join");
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        assert_eq!(json(&bytes)["following"], true);
    }
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 1);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 1);
    assert_eq!(relationship(&srv, &alice, &bob_id).await["following"], true);

    // 8 concurrent unfollows must land exactly zero follows.
    let unfollow_uri = format!("/api/v1/accounts/{bob_id}/unfollow");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = router.clone();
        let uri = unfollow_uri.clone();
        let token = alice.clone();
        handles.push(tokio::spawn(async move {
            send(&router, form_request("POST", &uri, "", Some(&token))).await
        }));
    }
    for handle in handles {
        let (status, _h, bytes) = handle.await.expect("join");
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        assert_eq!(json(&bytes)["following"], false);
    }
    assert_eq!(account_show(&srv, &bob_id).await["followers_count"], 0);
    assert_eq!(account_show(&srv, &alice_id).await["following_count"], 0);
    assert_eq!(relationship(&srv, &alice, &bob_id).await["following"], false);
}
