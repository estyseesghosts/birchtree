//! Phase 3 interaction contract tests: favourites, bookmarks, reblogs, pins,
//! polls and quotes + interaction policy, driven through the real router with
//! the pinned Mastodon v4.7.3 semantics (exact status codes, error bodies and
//! counter transitions).

mod common;

use std::collections::HashMap;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::{AccountStore, OAuthStore};

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

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=Phase3+Tests&redirect_uris={}&scopes={}",
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

/// Register an account through the app and return its user token.
async fn register_account(srv: &TestServer, app: &Value, username: &str) -> String {
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

/// Every scope the Phase 3 endpoints of this file touch.
const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts \
read:follows write:follows read:statuses write:statuses read:favourites \
write:favourites read:bookmarks write:bookmarks read:blocks write:blocks \
read:mutes write:mutes read:notifications write:notifications";

/// A fresh server with one all-scopes app and a user token per name.
async fn users(names: &[&str]) -> (TestServer, HashMap<String, String>) {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let mut tokens = HashMap::new();
    for name in names {
        tokens.insert(name.to_string(), register_account(&srv, &app, name).await);
    }
    (srv, tokens)
}

fn tok<'a>(users: &'a HashMap<String, String>, name: &str) -> &'a str {
    users.get(name).expect("user token")
}

/// The account id of a local user (direct store read).
fn account_id(srv: &TestServer, name: &str) -> String {
    srv.store
        .account_by_handle(name, "example.social")
        .unwrap()
        .unwrap()
        .id
        .to_string()
}

/// `POST` form data and return `(status, json body)`.
async fn post(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, token)).await;
    (status, json(&bytes))
}

/// `POST` asserting a 200 and returning the body.
async fn post_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, json) = post(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {json}");
    json
}

/// `PUT` form data and return `(status, json body)`.
async fn put(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("PUT", path, body, token)).await;
    (status, json(&bytes))
}

/// `GET` asserting a 200 and returning the body.
async fn get_ok(srv: &TestServer, path: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) = send(&srv.router, get_request(path, token, None)).await;
    assert_eq!(status, StatusCode::OK, "GET {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// ---------------------------------------------------------------------------
// 1. Favourites.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn favourite_lifecycle_and_counter_idempotency() {
    let (srv, users) = users(&["alice", "bob", "carol"]).await;
    let (alice, bob, carol) = (tok(&users, "alice"), tok(&users, "bob"), tok(&users, "carol"));

    let created = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=hello+fav").await;
    let id = created["id"].as_str().unwrap().to_string();

    // Fav -> favourited:true, favourites_count:1.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/favourite"), Some(bob), "").await;
    assert_eq!(body["favourited"], true);
    assert_eq!(body["favourites_count"], 1);

    // Repeat fav is idempotent: the count stays 1.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/favourite"), Some(bob), "").await;
    assert_eq!(body["favourited"], true);
    assert_eq!(body["favourites_count"], 1);

    // Unfavourite -> 0 and favourited:false.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/unfavourite"), Some(bob), "").await;
    assert_eq!(body["favourited"], false);
    assert_eq!(body["favourites_count"], 0);

    // Unfavourite again is a 200 no-op; the count never goes negative.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/unfavourite"), Some(bob), "").await;
    assert_eq!(body["favourited"], false);
    assert_eq!(body["favourites_count"], 0);

    // Per-viewer flags are independent: carol's favourite is invisible to bob.
    post_ok(&srv, &format!("/api/v1/statuses/{id}/favourite"), Some(carol), "").await;
    let bob_view = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(bob)).await;
    assert_eq!(bob_view["favourited"], false);
    assert_eq!(bob_view["favourites_count"], 1);
    let carol_view = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(carol)).await;
    assert_eq!(carol_view["favourited"], true);
    assert_eq!(carol_view["favourites_count"], 1);
}

// ---------------------------------------------------------------------------
// 2. Bookmarks.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bookmark_flags_are_per_viewer() {
    let (srv, users) = users(&["alice", "bob"]).await;
    let (alice, bob) = (tok(&users, "alice"), tok(&users, "bob"));

    let created = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=bookmark+me").await;
    let id = created["id"].as_str().unwrap();

    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/bookmark"), Some(bob), "").await;
    assert_eq!(body["bookmarked"], true);

    // The author does not see bob's bookmark.
    let alice_view = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(alice)).await;
    assert_eq!(alice_view["bookmarked"], false);
    let bob_view = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(bob)).await;
    assert_eq!(bob_view["bookmarked"], true);

    // Unbookmark flips it back for bob only.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/unbookmark"), Some(bob), "").await;
    assert_eq!(body["bookmarked"], false);
    let bob_view = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(bob)).await;
    assert_eq!(bob_view["bookmarked"], false);
}

// ---------------------------------------------------------------------------
// 3. Reblogs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reblog_creates_wrapper_and_unreblog_restores() {
    let (srv, users) = users(&["alice", "bob"]).await;
    let (alice, bob) = (tok(&users, "alice"), tok(&users, "bob"));

    let created = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=boost+me").await;
    let original_id = created["id"].as_str().unwrap().to_string();

    // Bob favourites the ORIGINAL first: the wrapper's per-viewer flags must
    // mirror the viewer's relation to the ORIGINAL (pinned Mastodon:
    // `Account#favourited?/#reblogged?` resolve through `status.proper`).
    post_ok(&srv, &format!("/api/v1/statuses/{original_id}/favourite"), Some(bob), "").await;

    // Reblog answers 200 with the NEW wrapper record wrapping the original.
    let wrapper =
        post_ok(&srv, &format!("/api/v1/statuses/{original_id}/reblog"), Some(bob), "").await;
    let wrapper_id = wrapper["id"].as_str().unwrap().to_string();
    assert_ne!(wrapper_id, original_id, "reblog is a new record");
    assert_eq!(wrapper["account"]["username"], "bob");
    assert_eq!(wrapper["reblog"]["id"], original_id.as_str());
    assert_eq!(wrapper["reblogs_count"], 0);
    assert_eq!(wrapper["favourites_count"], 0);
    // The wrapper's per-viewer flags mirror the viewer's relation to the
    // ORIGINAL (Mastodon keys reblogs/favourites by the proper status).
    assert_eq!(wrapper["reblogged"], true);
    assert_eq!(wrapper["favourited"], true);
    // The nested original carries the live counters and flags too.
    assert_eq!(wrapper["reblog"]["reblogs_count"], 1);
    assert_eq!(wrapper["reblog"]["favourited"], true);
    assert_eq!(wrapper["reblog"]["reblogged"], true);

    // The original moves 0 -> 1 and shows the viewer's flags.
    let view = get_ok(&srv, &format!("/api/v1/statuses/{original_id}"), Some(bob)).await;
    assert_eq!(view["reblogs_count"], 1);
    assert_eq!(view["reblogged"], true);
    assert_eq!(view["favourited"], true);

    // Re-reblogging is idempotent: the SAME wrapper id, count stays 1.
    let again =
        post_ok(&srv, &format!("/api/v1/statuses/{original_id}/reblog"), Some(bob), "").await;
    assert_eq!(again["id"], wrapper_id.as_str());
    let view = get_ok(&srv, &format!("/api/v1/statuses/{original_id}"), Some(bob)).await;
    assert_eq!(view["reblogs_count"], 1);

    // Unreblog answers 200 with the ORIGINAL, count back to 0.
    let original =
        post_ok(&srv, &format!("/api/v1/statuses/{original_id}/unreblog"), Some(bob), "").await;
    assert_eq!(original["id"], original_id.as_str());
    assert_eq!(original["reblogs_count"], 0);
    assert_eq!(original["reblogged"], false);

    let view = get_ok(&srv, &format!("/api/v1/statuses/{original_id}"), Some(bob)).await;
    assert_eq!(view["reblogs_count"], 0);
    assert_eq!(view["reblogged"], false);
    // The favourite is untouched by the unreblog.
    assert_eq!(view["favourited"], true);
    assert_eq!(view["favourites_count"], 1);
}

// ---------------------------------------------------------------------------
// 4. Visibility gates on interactions.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn interaction_visibility_gates() {
    let (srv, users) = users(&["alice", "bob"]).await;
    let (alice, bob) = (tok(&users, "alice"), tok(&users, "bob"));
    let bob_id = account_id(&srv, "bob");

    // Favourite of an invisible (private, non-follower) status -> 404.
    let priv_status = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=followers+only&visibility=private",
    )
    .await;
    let priv_id = priv_status["id"].as_str().unwrap();
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{priv_id}/favourite"), Some(bob), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Not Found");

    // Bob follows alice: the private status becomes fetchable, but a non-author
    // still may not reblog it -> 403 exact message.
    post_ok(&srv, &format!("/api/v1/accounts/{}/follow", account_id(&srv, "alice")), Some(bob), "").await;
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{priv_id}/reblog"), Some(bob), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is not allowed");

    // Reblogging a DIRECT status (as a mentioned recipient) -> 403 exact.
    let direct = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=hi+@bob&visibility=direct",
    )
    .await;
    let direct_id = direct["id"].as_str().unwrap();
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{direct_id}/reblog"), Some(bob), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "This action is not allowed");

    // After a block the author's public status vanishes for the blocked user.
    let public = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=public+post").await;
    let public_id = public["id"].as_str().unwrap();
    post_ok(&srv, &format!("/api/v1/accounts/{bob_id}/block"), Some(alice), "").await;
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{public_id}/favourite"), Some(bob), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Not Found");
}

// ---------------------------------------------------------------------------
// 5. Pins.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pin_rules_and_pinned_list() {
    let (srv, users) = users(&["alice", "bob", "carol"]).await;
    let (alice, bob) = (tok(&users, "alice"), tok(&users, "bob"));
    let alice_id = account_id(&srv, "alice");

    let mut pins: Vec<String> = Vec::new();
    for n in 1..=6 {
        let created = post_ok(
            &srv,
            "/api/v1/statuses",
            Some(alice),
            &format!("status=pin+{n}"),
        )
        .await;
        pins.push(created["id"].as_str().unwrap().to_string());
    }

    // Someone else's post cannot be pinned (pinned Mastodon v4.7.3: the
    // StatusPin validator raises RecordInvalid -> 422 with the
    // "Validation failed: " prefix).
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{}/pin", pins[0]), Some(bob), "").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: Someone else's post cannot be pinned");

    // A boost cannot be pinned.
    let boost = post_ok(&srv, &format!("/api/v1/statuses/{}/reblog", pins[0]), Some(bob), "").await;
    let boost_id = boost["id"].as_str().unwrap();
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{boost_id}/pin"), Some(bob), "").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: A boost cannot be pinned");

    // A direct post cannot be pinned.
    let direct = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=dm&visibility=direct",
    )
    .await;
    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{}/pin", direct["id"].as_str().unwrap()),
        Some(alice),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        "Validation failed: Posts that are only visible to mentioned users cannot be pinned"
    );

    // Pin five (sleeps keep pin timestamps distinct: the pinned list orders
    // by pin time descending).
    for id in &pins[..5] {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/pin"), Some(alice), "").await;
        assert_eq!(body["pinned"], true);
    }

    // The sixth pin hits the limit.
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{}/pin", pins[5]), Some(alice), "").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        "Validation failed: You have already pinned the maximum number of posts"
    );

    // `pinned` appears for the author and is ABSENT for anyone else.
    let alice_view = get_ok(&srv, &format!("/api/v1/statuses/{}", pins[0]), Some(alice)).await;
    assert_eq!(alice_view["pinned"], true);
    let bob_view = get_ok(&srv, &format!("/api/v1/statuses/{}", pins[0]), Some(bob)).await;
    assert!(bob_view.get("pinned").is_none(), "no pinned key for non-author");

    // The pinned list orders by pin time descending: last pinned first.
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses?pinned=true"),
        Some(alice),
    )
    .await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![pins[4].as_str(), &pins[3], &pins[2], &pins[1], &pins[0]]);

    // Unpin removes it from the list; a second unpin is a 200 no-op.
    let body = post_ok(&srv, &format!("/api/v1/statuses/{}/unpin", pins[4]), Some(alice), "").await;
    assert_eq!(body["pinned"], false);
    let body = post_ok(&srv, &format!("/api/v1/statuses/{}/unpin", pins[4]), Some(alice), "").await;
    assert_eq!(body["pinned"], false);
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses?pinned=true"),
        Some(alice),
    )
    .await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![pins[3].as_str(), &pins[2], &pins[1], &pins[0]]);

    // `exclude_reblogs` on the account timeline is unaffected by pins.
    let carol_tok = tok(&users, "carol");
    let carol_id = account_id(&srv, "carol");
    post_ok(&srv, &format!("/api/v1/statuses/{}/reblog", pins[0]), Some(carol_tok), "").await;
    let with_reblogs = get_ok(
        &srv,
        &format!("/api/v1/accounts/{carol_id}/statuses"),
        Some(carol_tok),
    )
    .await;
    assert_eq!(with_reblogs.as_array().unwrap().len(), 1);
    let excluded = get_ok(
        &srv,
        &format!("/api/v1/accounts/{carol_id}/statuses?exclude_reblogs=true"),
        Some(carol_tok),
    )
    .await;
    assert_eq!(excluded.as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// 6. Polls.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_vote_contract() {
    let (srv, users) = users(&["alice", "bob", "carol"]).await;
    let (alice, bob, carol) = (tok(&users, "alice"), tok(&users, "bob"), tok(&users, "carol"));

    // A single-choice, hide_totals poll.
    let poll_status = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=which+one&poll[multiple]=false&poll[hide_totals]=true\
&poll[expires_in]=3600&poll[options][]=tea&poll[options][]=coffee",
    )
    .await;
    let poll_id = poll_status["poll"]["id"].as_str().unwrap().to_string();

    // Missing `choices` -> 400 with the exact body.
    let (status, body) = post(&srv, &format!("/api/v1/polls/{poll_id}/votes"), Some(bob), "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({"error": "param is missing or the value is empty: choices"}));

    // Author self-vote -> 422.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(alice),
        "choices[]=0",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: You cannot vote in your own polls");

    // Out-of-range choice -> 422.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=9",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: The chosen vote option does not exist");

    // A valid vote; hide_totals masks per-option counts but poll-level counts
    // stay visible.
    let vote = post_ok(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=0",
    )
    .await;
    assert_eq!(vote["voted"], true);
    assert_eq!(vote["own_votes"][0], 0);
    assert_eq!(vote["expired"], false);
    assert!(vote["options"][0]["votes_count"].is_null());
    assert!(vote["options"][1]["votes_count"].is_null());
    assert_eq!(vote["votes_count"], 1);
    assert_eq!(vote["voters_count"], 1);

    // A second vote on a single-choice poll -> 422.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=1",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: You have already voted on this poll");

    // The read surface agrees with the vote response.
    let shown = get_ok(&srv, &format!("/api/v1/polls/{poll_id}"), Some(bob)).await;
    assert_eq!(shown["voted"], true);
    assert_eq!(shown["own_votes"][0], 0);
    assert!(shown["options"][0]["votes_count"].is_null());
    assert_eq!(shown["votes_count"], 1);

    // A multiple-choice poll ADDs new choices and rejects already-cast ones.
    let multi = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=multi&poll[multiple]=true&poll[expires_in]=3600\
&poll[options][]=alpha&poll[options][]=beta&poll[options][]=gamma",
    )
    .await;
    let multi_id = multi["poll"]["id"].as_str().unwrap();

    let vote = post_ok(&srv, &format!("/api/v1/polls/{multi_id}/votes"), Some(bob), "choices[]=0").await;
    assert_eq!(vote["own_votes"][0], 0);
    assert_eq!(vote["votes_count"], 1);
    assert_eq!(vote["voters_count"], 1);

    let vote = post_ok(&srv, &format!("/api/v1/polls/{multi_id}/votes"), Some(bob), "choices[]=1").await;
    assert_eq!(vote["own_votes"], serde_json::json!([0, 1]));
    assert_eq!(vote["votes_count"], 2);
    assert_eq!(vote["voters_count"], 1, "voters_count counts ballots, not choices");

    // Re-voting an ALREADY-CAST choice is rejected; new choices would be added.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{multi_id}/votes"),
        Some(bob),
        "choices[]=0&choices[]=2",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "Validation failed: You have already voted on this poll");

    // A different voter is unaffected.
    let vote = post_ok(&srv, &format!("/api/v1/polls/{multi_id}/votes"), Some(carol), "choices[]=2").await;
    assert_eq!(vote["voters_count"], 2);
}

// ---------------------------------------------------------------------------
// 7. Quotes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quote_lifecycle() {
    let (srv, users) = users(&["alice", "bob", "carol"]).await;
    let (alice, bob, carol) = (tok(&users, "alice"), tok(&users, "bob"), tok(&users, "carol"));

    // carol's status T; alice's status S quotes T (so S carries a quote of
    // its own, exercising the shallow nesting when bob quotes S).
    let t = post_ok(&srv, "/api/v1/statuses", Some(carol), "status=T+post").await;
    let t_id = t["id"].as_str().unwrap().to_string();
    let s = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        &format!("status=S+post&quoted_status_id={t_id}"),
    )
    .await;
    let s_id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["quote"]["state"], "accepted");

    // A local quote is auto-accepted with the quoted status nested; the
    // nested status's OWN quote field is shallow ({state, quoted_status_id}).
    let quoting = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(bob),
        &format!("status=bob+quotes&quoted_status_id={s_id}"),
    )
    .await;
    let quoting_id = quoting["id"].as_str().unwrap().to_string();
    assert_eq!(quoting["quote"]["state"], "accepted");
    assert_eq!(quoting["quote"]["quoted_status"]["id"], s_id.as_str());
    let inner_quote = &quoting["quote"]["quoted_status"]["quote"];
    assert_eq!(inner_quote["state"], "accepted");
    assert_eq!(inner_quote["quoted_status_id"], t_id.as_str());
    assert!(
        inner_quote.get("quoted_status").is_none(),
        "one nesting level down the quote is shallow"
    );

    // quotes_count +1 on the quoted status.
    let view = get_ok(&srv, &format!("/api/v1/statuses/{s_id}"), Some(alice)).await;
    assert_eq!(view["quotes_count"], 1);

    // The quotes list contains the accepted quoting status.
    let list = get_ok(&srv, &format!("/api/v1/statuses/{s_id}/quotes"), Some(alice)).await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|st| st["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![quoting_id.as_str()]);

    // The QUOTING status's author may not revoke (only the quoted author).
    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{s_id}/quotes/{quoting_id}/revoke"),
        Some(bob),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is not allowed");

    // Revocation by the quoted author: state revoked, content dropped.
    let revoked = post_ok(
        &srv,
        &format!("/api/v1/statuses/{s_id}/quotes/{quoting_id}/revoke"),
        Some(alice),
        "",
    )
    .await;
    assert_eq!(revoked["id"], quoting_id.as_str());
    assert_eq!(revoked["quote"]["state"], "revoked");
    assert!(revoked["quote"]["quoted_status"].is_null());

    // quotes_count back to 0; the list is empty again.
    let view = get_ok(&srv, &format!("/api/v1/statuses/{s_id}"), Some(alice)).await;
    assert_eq!(view["quotes_count"], 0);
    let list = get_ok(&srv, &format!("/api/v1/statuses/{s_id}/quotes"), Some(alice)).await;
    assert_eq!(list.as_array().unwrap().len(), 0);

    // An unknown quote pair -> 404 "Record not found".
    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{s_id}/quotes/{t_id}/revoke"),
        Some(alice),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Record not found");

    // Interaction policy round-trip on the author's own status.
    let (status, body) = put(
        &srv,
        &format!("/api/v1/statuses/{s_id}/interaction_policy"),
        Some(alice),
        "quote_approval_policy=followers",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quote_approval"]["automatic"], serde_json::json!(["followers"]));

    let (status, body) = put(
        &srv,
        &format!("/api/v1/statuses/{s_id}/interaction_policy"),
        Some(alice),
        "quote_approval_policy=nobody",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quote_approval"]["automatic"], serde_json::json!([]));

    // Invalid value -> 422 {"error":"Record invalid"}.
    let (status, body) = put(
        &srv,
        &format!("/api/v1/statuses/{s_id}/interaction_policy"),
        Some(alice),
        "quote_approval_policy=bogus",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body, serde_json::json!({"error": "Record invalid"}));

    // Someone else's status -> 403.
    let (status, body) = put(
        &srv,
        &format!("/api/v1/statuses/{s_id}/interaction_policy"),
        Some(bob),
        "quote_approval_policy=public",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "This action is not allowed");

    // After `nobody`, a NEW quote attempt answers the pinned 404 message.
    let (status, body) = post(
        &srv,
        "/api/v1/statuses",
        Some(carol),
        &format!("status=no+quote&quoted_status_id={s_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "The post you are trying to quote does not appear to exist.");

    // Quoting a non-visible status answers the same 404.
    let hidden = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(carol),
        "status=hidden&visibility=private",
    )
    .await;
    let (status, body) = post(
        &srv,
        "/api/v1/statuses",
        Some(bob),
        &format!(
            "status=steal+quote&quoted_status_id={}",
            hidden["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "The post you are trying to quote does not appear to exist.");
}

// ---------------------------------------------------------------------------
// 8. Auth error shapes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn interaction_error_shapes() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice = register_account(&srv, &app, "alice").await;
    let bob = register_account(&srv, &app, "bob").await;
    let created = post_ok(&srv, "/api/v1/statuses", Some(&alice), "status=auth+shapes").await;
    let id = created["id"].as_str().unwrap();

    // Missing token -> 401 with the exact body.
    let (status, body) = post(&srv, &format!("/api/v1/statuses/{id}/favourite"), None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, serde_json::json!({"error": "The access token is invalid"}));

    // A user token with only `read` -> 403 (outside the authorized scopes).
    let app = srv
        .store
        .app_by_client_id(app["client_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let bob_account = srv
        .store
        .account_by_handle("bob", "example.social")
        .unwrap()
        .unwrap();
    let (read_token, _record) = birchtree::application::auth::issue_user_token(
        &*srv.store,
        app.id,
        bob_account.id,
        vec!["read".to_string()],
        common::T0,
    )
    .unwrap();
    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{id}/favourite"),
        Some(&read_token),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, serde_json::json!({"error": "This action is outside the authorized scopes"}));

    // An app-only (client-credentials) token -> 422 requires-user.
    let app2 = register_app(&srv, ALL_SCOPES).await;
    let app_token = app_token(&srv, &app2, "write").await;
    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{id}/favourite"),
        Some(&app_token),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, serde_json::json!({"error": "This method requires an authenticated user"}));

    // The `bob` token itself still works (sanity for the read-scope token).
    let body = post_ok(&srv, &format!("/api/v1/statuses/{id}/favourite"), Some(&bob), "").await;
    assert_eq!(body["favourited"], true);
}
