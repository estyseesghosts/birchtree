//! Phase 3 status endpoint contract tests: create/show DTOs, viewer fields,
//! validation errors, reply/quote resolution, visibility policy, context,
//! source/history, multi-fetch, idempotency, deletion, editing, account
//! status lists and favourited/reblogged-by lists.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::application::ports::AccountStore;
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

/// Register an account and return `(user_token, token_body)`. The user token
/// carries the app's full scope set.
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
                         read:follows write:follows read:statuses write:statuses \
                         read:favourites write:favourites read:bookmarks write:bookmarks \
                         read:blocks write:blocks read:mutes write:mutes";

/// `POST /api/v1/statuses` with an optional `Idempotency-Key` header.
fn post_status_request(body: &str, token: &str, idempotency_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", format!("Bearer {token}"));
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// Create a status as `token`, asserting 200 and returning the body.
async fn create(srv: &TestServer, token: &str, body: &str) -> Value {
    let (status, _h, bytes) = send(&srv.router, post_status_request(body, token, None)).await;
    assert_eq!(status, StatusCode::OK, "create: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// GET a URI, asserting 200 and returning the parsed body.
async fn get_ok(srv: &TestServer, uri: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) = send(&srv.router, get_request(uri, token, None)).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// GET a URI and assert the status code only.
async fn code(srv: &TestServer, uri: &str, token: Option<&str>) -> StatusCode {
    let (status, _h, _bytes) = send(&srv.router, get_request(uri, token, None)).await;
    status
}

/// The `error` string of an error body.
fn error_of(bytes: &[u8]) -> String {
    json(bytes)["error"]
        .as_str()
        .unwrap_or_else(|| panic!("no error string in body: {}", String::from_utf8_lossy(bytes)))
        .to_string()
}

/// Sorted object key set, for exact-shape assertions.
fn key_set(value: &Value) -> Vec<String> {
    // Sorted: serde_json's map order depends on the `preserve_order` feature,
    // which the federation crate enables. The key set is what the test pins.
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("not a JSON object: {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The local account id for a username.
fn account_id(srv: &TestServer, username: &str) -> String {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

async fn statuses_count(srv: &TestServer, account_id: &str) -> u64 {
    let account = get_ok(srv, &format!("/api/v1/accounts/{account_id}"), None).await;
    account["statuses_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("statuses_count not numeric: {account}"))
}

// ---------------------------------------------------------------------------
// 1. Created status DTO: exact key set and value types.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_returns_contract_complete_status_dto() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _body) = register_account(&srv, &app, "alice").await;

    let created = create(
        &srv,
        &alice,
        &format!("status={}&language=en", enc("hello <b>world</b> & \"friends\"")),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // The anonymous rendering: no per-viewer fields, no `pinned`, `content`
    // (not `text`).
    let status = get_ok(&srv, &format!("/api/v1/statuses/{id}"), None).await;
    let expected_keys: Vec<String> = [
        "account",
        "application",
        "card",
        "content",
        "created_at",
        "edited_at",
        "emojis",
        "favourites_count",
        "id",
        "in_reply_to_account_id",
        "in_reply_to_id",
        "language",
        "media_attachments",
        "mentions",
        "poll",
        "quote",
        "quote_approval",
        "quotes_count",
        "reblog",
        "reblogs_count",
        "replies_count",
        "sensitive",
        "spoiler_text",
        "tagged_collections",
        "tags",
        "uri",
        "url",
        "visibility",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        key_set(&status),
        expected_keys,
        "anonymous status key set differs from the pinned serializer shape: {status}"
    );

    // id: decimal string.
    assert!(
        id.chars().all(|c| c.is_ascii_digit()) && id.parse::<u64>().is_ok(),
        "id is not a decimal string: {id}"
    );
    // Counts are JSON numbers; a fresh status has zeros.
    for field in [
        "replies_count",
        "reblogs_count",
        "favourites_count",
        "quotes_count",
    ] {
        assert_eq!(status[field], 0, "{field} should be 0: {status}");
    }
    assert_eq!(status["edited_at"], Value::Null, "edited_at must be null");
    assert_eq!(status["language"], "en");
    assert_eq!(status["visibility"], "public");
    assert_eq!(status["sensitive"], false);
    assert_eq!(status["spoiler_text"], "");
    assert_eq!(status["in_reply_to_id"], Value::Null);
    assert_eq!(status["in_reply_to_account_id"], Value::Null);
    // Rendered, HTML-escaped content wrapped in <p>.
    assert_eq!(
        status["content"],
        "<p>hello &lt;b&gt;world&lt;/b&gt; &amp; &quot;friends&quot;</p>"
    );
    // Application: exactly {name, website: null}.
    assert_eq!(
        status["application"],
        serde_json::json!({ "name": "Test App", "website": Value::Null })
    );
    // Null associations.
    assert_eq!(status["reblog"], Value::Null);
    assert_eq!(status["card"], Value::Null);
    assert_eq!(status["poll"], Value::Null);
    assert_eq!(status["quote"], Value::Null);
    // Array associations.
    for field in [
        "emojis",
        "media_attachments",
        "mentions",
        "tags",
        "tagged_collections",
    ] {
        assert!(
            status[field].is_array(),
            "{field} should be an array: {status}"
        );
    }
    // Interaction policy for an anonymous viewer.
    assert_eq!(
        status["quote_approval"],
        serde_json::json!({
            "automatic": ["public"],
            "manual": [],
            "current_user": "denied",
        })
    );
    // uri/url live under the public base URL.
    assert!(
        status["uri"].as_str().unwrap().starts_with("https://example.social/"),
        "uri: {status}"
    );
    assert!(
        status["url"].as_str().unwrap().starts_with("https://example.social/"),
        "url: {status}"
    );
}

// ---------------------------------------------------------------------------
// 2. Per-viewer fields appear only with a token.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn viewer_fields_appear_only_with_token() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let created = create(&srv, &alice, "status=hello+viewer").await;
    let id = created["id"].as_str().unwrap();

    // Anonymous: none of the per-viewer fields, no pinned.
    let anon = get_ok(&srv, &format!("/api/v1/statuses/{id}"), None).await;
    for field in [
        "favourited",
        "reblogged",
        "muted",
        "bookmarked",
        "filtered",
        "pinned",
    ] {
        assert!(
            anon.get(field).is_none(),
            "{field} must be absent for an anonymous viewer: {anon}"
        );
    }

    // A non-interacting viewer token: the four interaction flags (false) and
    // filtered ([]); `pinned` only when pinnable (bob is not the author).
    let viewer = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(&bob)).await;
    for field in ["favourited", "reblogged", "muted", "bookmarked"] {
        assert_eq!(
            viewer.get(field),
            Some(&Value::Bool(false)),
            "{field} should be present and false for a viewer token: {viewer}"
        );
    }
    assert_eq!(viewer["filtered"], serde_json::json!([]));
    assert!(
        viewer.get("pinned").is_none(),
        "pinned must be absent for a non-author viewer: {viewer}"
    );

    // The author: `pinned` present (public status, not a reblog).
    let author = get_ok(&srv, &format!("/api/v1/statuses/{id}"), Some(&alice)).await;
    assert_eq!(
        author.get("pinned"),
        Some(&Value::Bool(false)),
        "pinned should be present (false) for the author: {author}"
    );
}

// ---------------------------------------------------------------------------
// 3. Creation validation errors (Mastodon message strings).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_validation_errors() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;

    // Blank text.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=&visibility=public", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_of(&bytes), "Validation failed: Text can't be blank");

    // Length boundary: URLs count as a fixed 23 characters. 476 chars + " "
    // + one URL = exactly 500 (allowed); 477 + " " + URL = 501 (rejected).
    let url = format!("https://example.com/{}", "x".repeat(40));
    let at_limit = format!("{} {}", "a".repeat(476), url);
    assert_eq!(
        birchtree::application::statuses::count_length(&at_limit),
        500,
        "boundary fixture"
    );
    create(
        &srv,
        &alice,
        &format!("status={}&visibility=public", enc(&at_limit)),
    )
    .await;

    let over_limit = format!("{} {}", "a".repeat(477), url);
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request(
            &format!("status={}&visibility=public", enc(&over_limit)),
            &alice,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("character limit of 500 exceeded"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );

    // Unknown visibility value.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=hi&visibility=bogus", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("Visibility"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );

    // Poll with a single option.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request(
            "status=vote&poll[options][]=only&poll[expires_in]=3600",
            &alice,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("must have more than one item"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );

    // Poll without an expiration.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=vote&poll[options][]=a&poll[options][]=b", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("Deadline can't be blank"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );

    // Poll expiring sooner than the 5-minute minimum.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request(
            "status=vote&poll[options][]=a&poll[options][]=b&poll[expires_in]=60",
            &alice,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("Deadline is too soon"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );
}

// ---------------------------------------------------------------------------
// 4. Reply and quote resolution errors.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reply_and_quote_resolution_errors() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;

    // Unknown numeric reply parent.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=hi&in_reply_to_id=999999999999999999", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error_of(&bytes),
        "The post you are trying to reply to does not appear to exist."
    );

    // Unparsable reply parent behaves the same (Mastodon `Status.find`).
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=hi&in_reply_to_id=bogus", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error_of(&bytes),
        "The post you are trying to reply to does not appear to exist."
    );

    // Unknown quoted status.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=hi&quoted_status_id=999999999999999999", &alice, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error_of(&bytes),
        "The post you are trying to quote does not appear to exist."
    );

    // Blank text with an unknown media attachment: the media check fires.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request(
            "status=&media_ids[]=999999999999999999",
            &alice,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error_of(&bytes).contains("not found or already attached"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bytes)
    );
}

// ---------------------------------------------------------------------------
// 5. Visibility policy matrix.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn visibility_policy_matrix() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let (carol, _b) = register_account(&srv, &app, "carol").await;
    let (dave, _b) = register_account(&srv, &app, "dave").await;
    let alice_id = account_id(&srv, "alice");
    let dave_id = account_id(&srv, "dave");

    let pub_id = create(&srv, &alice, "status=public+post&visibility=public").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let unlisted_id = create(&srv, &alice, "status=unlisted+post&visibility=unlisted").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let private_id = create(&srv, &alice, "status=private+post&visibility=private").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let direct_id = create(&srv, &alice, "status=hi+@carol&visibility=direct").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let show = |id: &str| format!("/api/v1/statuses/{id}");

    // Anonymous: public/unlisted only.
    assert_eq!(code(&srv, &show(&pub_id), None).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&unlisted_id), None).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&private_id), None).await, StatusCode::NOT_FOUND);
    assert_eq!(code(&srv, &show(&direct_id), None).await, StatusCode::NOT_FOUND);

    // Unrelated bob (not yet following): same as anonymous.
    assert_eq!(code(&srv, &show(&pub_id), Some(&bob)).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&unlisted_id), Some(&bob)).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&private_id), Some(&bob)).await, StatusCode::NOT_FOUND);
    assert_eq!(code(&srv, &show(&direct_id), Some(&bob)).await, StatusCode::NOT_FOUND);

    // Bob follows alice: private opens up, direct stays closed.
    let rel = json(
        &send(
            &srv.router,
            form_request(
                "POST",
                &format!("/api/v1/accounts/{alice_id}/follow"),
                "",
                Some(&bob),
            ),
        )
        .await
        .2,
    );
    assert_eq!(rel["following"], true, "bob should follow alice: {rel}");
    assert_eq!(code(&srv, &show(&private_id), Some(&bob)).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&direct_id), Some(&bob)).await, StatusCode::NOT_FOUND);

    // Mentioned carol: direct opens; private stays closed (not a follower).
    assert_eq!(code(&srv, &show(&direct_id), Some(&carol)).await, StatusCode::OK);
    assert_eq!(code(&srv, &show(&private_id), Some(&carol)).await, StatusCode::NOT_FOUND);
    assert_eq!(code(&srv, &show(&pub_id), Some(&carol)).await, StatusCode::OK);

    // The author sees everything.
    for id in [&pub_id, &unlisted_id, &private_id, &direct_id] {
        assert_eq!(code(&srv, &show(id), Some(&alice)).await, StatusCode::OK);
    }

    // Reblogging a direct message is forbidden.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{direct_id}/reblog"), "", Some(&carol)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(error_of(&bytes), "This action is not allowed");

    // Block boundary: dave sees alice's public status until alice blocks him.
    assert_eq!(code(&srv, &show(&pub_id), Some(&dave)).await, StatusCode::OK);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{dave_id}/block"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["blocking"], true);
    assert_eq!(code(&srv, &show(&pub_id), Some(&dave)).await, StatusCode::NOT_FOUND);
    assert_eq!(code(&srv, &show(&unlisted_id), Some(&dave)).await, StatusCode::NOT_FOUND);
    // Anonymous visibility is unaffected by a viewer-specific block.
    assert_eq!(code(&srv, &show(&pub_id), None).await, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 6. Context: ancestors oldest-first, descendants breadth-first.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn context_ancestors_descendants() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;

    let root = create(&srv, &alice, "status=thread+root").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = create(
        &srv,
        &bob,
        &format!("status=first+reply&in_reply_to_id={root}"),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let deep = create(
        &srv,
        &bob,
        &format!("status=deep+reply&in_reply_to_id={reply}"),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let ctx = get_ok(&srv, &format!("/api/v1/statuses/{deep}/context"), Some(&bob)).await;
    let ancestors: Vec<String> = ctx["ancestors"]
        .as_array()
        .expect("ancestors array")
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ancestors, vec![root.clone(), reply.clone()], "ancestors: {ctx}");

    let ctx = get_ok(&srv, &format!("/api/v1/statuses/{root}/context"), Some(&bob)).await;
    let descendants: Vec<String> = ctx["descendants"]
        .as_array()
        .expect("descendants array")
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(descendants, vec![reply, deep], "descendants: {ctx}");

    // Anonymous callers get the same small thread (within the anon caps).
    let ctx = get_ok(&srv, &format!("/api/v1/statuses/{root}/context"), None).await;
    let descendants: Vec<String> = ctx["descendants"]
        .as_array()
        .expect("descendants array")
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(descendants.len(), 2, "anon descendants: {ctx}");
}

// ---------------------------------------------------------------------------
// 7. Source and history endpoints.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn source_and_history_endpoints() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let created = create(&srv, &alice, "status=original+text").await;
    let id = created["id"].as_str().unwrap();

    // Source requires a token.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{id}/source"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // With a read:statuses-capable token: exactly {id, text, spoiler_text}.
    let source = get_ok(&srv, &format!("/api/v1/statuses/{id}/source"), Some(&alice)).await;
    assert_eq!(
        key_set(&source),
        vec!["id".to_string(), "spoiler_text".to_string(), "text".to_string()],
        "source key set: {source}"
    );
    assert_eq!(source["text"], "original text");
    assert_eq!(source["spoiler_text"], "");

    // History also requires a token.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{id}/history"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Never edited: a single synthesized version 0 with the serializer's
    // exact key set (no id/language/counts/quote).
    let history = get_ok(&srv, &format!("/api/v1/statuses/{id}/history"), Some(&alice)).await;
    let entries = history.as_array().expect("history array");
    assert_eq!(entries.len(), 1, "synthesized history: {history}");
    let expected_keys: Vec<String> = [
        "account",
        "content",
        "created_at",
        "emojis",
        "media_attachments",
        "poll",
        "sensitive",
        "spoiler_text",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        key_set(&entries[0]),
        expected_keys,
        "history entry key set: {}",
        entries[0]
    );
    assert_eq!(entries[0]["content"], "<p>original text</p>");

    // After one edit: two entries, version 0 keeps the original content.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            "status=edited+text",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let history = get_ok(&srv, &format!("/api/v1/statuses/{id}/history"), Some(&alice)).await;
    let entries = history.as_array().expect("history array");
    assert_eq!(entries.len(), 2, "history after one edit: {history}");
    assert_eq!(entries[0]["content"], "<p>original text</p>", "version 0: {history}");
    assert_eq!(entries[1]["content"], "<p>edited text</p>", "version 1: {history}");
    for entry in entries {
        assert!(
            entry.get("quote").is_none(),
            "history entries must not carry a quote key: {entry}"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. Multi-fetch: visibility filtering, ordering, limits, dedup.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_fetch() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (carol, _b) = register_account(&srv, &app, "carol").await;

    let a = create(&srv, &alice, "status=first").await["id"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let b = create(&srv, &alice, "status=second").await["id"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let c = create(&srv, &carol, "status=secret&visibility=private").await["id"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();

    // Invisible ids are silently omitted; result is id-descending.
    let uri = format!("/api/v1/statuses?id[]={a}&id[]={b}&id[]={c}");
    let statuses = get_ok(&srv, &uri, None).await;
    let ids: Vec<u64> = statuses
        .as_array()
        .expect("array")
        .iter()
        .map(|s| s["id"].as_str().unwrap().parse::<u64>().unwrap())
        .collect();
    assert_eq!(ids, vec![b, a], "multi-fetch: {statuses}");

    // More than 20 ids is a bare Mastodon::ValidationError.
    let query: Vec<String> = (1..=21).map(|i| format!("id[]={i}")).collect();
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses?{}", query.join("&")), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes),
        serde_json::json!({ "error": "Mastodon::ValidationError" })
    );

    // Duplicate ids are deduplicated (Mastodon `Array(...).uniq`).
    let uri = format!("/api/v1/statuses?id[]={a}&id[]={a}&id[]={b}");
    let statuses = get_ok(&srv, &uri, None).await;
    assert_eq!(
        statuses.as_array().expect("array").len(),
        2,
        "duplicate ids must dedup: {statuses}"
    );
}

// ---------------------------------------------------------------------------
// 9. Idempotency-Key replay semantics.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn idempotency_key_replays_and_is_per_account() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let alice_id = account_id(&srv, "alice");

    // First create with a key.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=retryable+post", &alice, Some("retry-42")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let first = json(&bytes);
    let first_id = first["id"].as_str().unwrap().to_string();

    // Replay with different text but the same key: the FIRST status returns.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=totally+different+text", &alice, Some("retry-42")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let replay = json(&bytes);
    assert_eq!(replay["id"], first_id, "replay: {replay}");
    assert_eq!(
        replay["content"],
        "<p>retryable post</p>",
        "replay must return the original status: {replay}"
    );

    // Keys are namespaced per account: bob with the SAME key creates anew.
    let (status, _h, bytes) = send(
        &srv.router,
        post_status_request("status=retryable+post", &bob, Some("retry-42")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let other = json(&bytes);
    assert_ne!(
        other["id"].as_str().unwrap(),
        first_id,
        "same key from another account must create a new status: {other}"
    );

    // The replay did not bump alice's statuses_count.
    assert_eq!(statuses_count(&srv, &alice_id).await, 1);
}

// ---------------------------------------------------------------------------
// 10. Deletion: tombstone body, 404 shapes, counter fixes, cascade.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_tombstones_and_returns_source() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let alice_id = account_id(&srv, "alice");
    let bob_id = account_id(&srv, "bob");

    let parent = create(&srv, &alice, "status=parent+post").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = create(
        &srv,
        &bob,
        &format!("status=a+reply&in_reply_to_id={parent}"),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let reblog = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{parent}/reblog"), "", Some(&bob)),
    )
    .await;
    assert_eq!(reblog.0, StatusCode::OK, "{}", String::from_utf8_lossy(&reblog.2));
    let reblog_id = json(&reblog.2)["id"].as_str().unwrap().to_string();

    let parent_json = get_ok(&srv, &format!("/api/v1/statuses/{parent}"), None).await;
    assert_eq!(parent_json["replies_count"], 1, "{parent_json}");
    assert_eq!(parent_json["reblogs_count"], 1, "{parent_json}");
    assert_eq!(statuses_count(&srv, &alice_id).await, 1);
    assert_eq!(statuses_count(&srv, &bob_id).await, 2);

    // Deleting the reply decrements the parent's replies_count.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/statuses/{reply}"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(statuses_count(&srv, &bob_id).await, 1);
    let parent_json = get_ok(&srv, &format!("/api/v1/statuses/{parent}"), None).await;
    assert_eq!(parent_json["replies_count"], 0, "{parent_json}");

    // Deleting the parent: 200 with source (`text`, no `content`).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/statuses/{parent}"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["text"], "parent post", "delete body: {body}");
    assert!(
        body.get("content").is_none(),
        "delete body must not carry content: {body}"
    );

    // GET afterwards: sub-route 404 shape.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{parent}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes), serde_json::json!({ "error": "Not Found" }));

    // Second DELETE: update/destroy 404 shape.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/statuses/{parent}"), "", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes), serde_json::json!({ "error": "Record not found" }));

    // The reblog of the deleted status is tombstoned too.
    assert_eq!(
        code(&srv, &format!("/api/v1/statuses/{reblog_id}"), None).await,
        StatusCode::NOT_FOUND
    );

    // Author counters: alice back to 0; bob's cascaded reblog decremented.
    assert_eq!(statuses_count(&srv, &alice_id).await, 0);
    assert_eq!(statuses_count(&srv, &bob_id).await, 0);
}

// ---------------------------------------------------------------------------
// 11. Editing: edited_at, no-op PUT, ownership, spoiler clearing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_updates_and_bumps_edits() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let created = create(
        &srv,
        &alice,
        "status=original+text&spoiler_text=CW",
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["spoiler_text"], "CW");
    assert_eq!(created["edited_at"], Value::Null);

    // A real edit sets edited_at.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            "status=edited+text",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let edited = json(&bytes);
    assert_eq!(edited["content"], "<p>edited text</p>");
    assert!(edited["edited_at"].is_string(), "edited_at set: {edited}");
    let edited_at = edited["edited_at"].as_str().unwrap().to_string();
    let history_len = get_ok(&srv, &format!("/api/v1/statuses/{id}/history"), Some(&alice))
        .await
        .as_array()
        .expect("history")
        .len();
    assert_eq!(history_len, 2);

    // A no-op PUT answers 200 without bumping edited_at or history.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            "status=edited+text",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let noop = json(&bytes);
    assert_eq!(
        noop["edited_at"].as_str().unwrap(),
        edited_at,
        "no-op PUT must not bump edited_at: {noop}"
    );
    let history_len = get_ok(&srv, &format!("/api/v1/statuses/{id}/history"), Some(&alice))
        .await
        .as_array()
        .expect("history")
        .len();
    assert_eq!(history_len, 2, "no-op PUT must not add a version");

    // Editing someone else's status: update/destroy 404 shape.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            "status=not+mine",
            Some(&bob),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes), serde_json::json!({ "error": "Record not found" }));

    // A present-but-blank spoiler_text clears the CW.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            "spoiler_text=",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let cleared = json(&bytes);
    assert_eq!(cleared["spoiler_text"], "", "CW cleared: {cleared}");
}

// ---------------------------------------------------------------------------
// 12. Account statuses: filters, pins, Link headers.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn account_statuses_pinned_and_filters() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let alice_id = account_id(&srv, "alice");

    let first = create(&srv, &alice, "status=first+post").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = create(
        &srv,
        &alice,
        &format!("status=self+reply&in_reply_to_id={first}"),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bob_status = create(&srv, &bob, "status=bob+original").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let boost = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{bob_status}/reblog"), "", Some(&alice)),
    )
    .await;
    assert_eq!(boost.0, StatusCode::OK, "{}", String::from_utf8_lossy(&boost.2));
    let boost_id = json(&boost.2)["id"].as_str().unwrap().to_string();

    let ids_of = |list: &Value| -> Vec<String> {
        list.as_array()
            .expect("array")
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    };

    // Baseline: id-descending, includes the reply and the boost.
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses"),
        Some(&alice),
    )
    .await;
    assert_eq!(ids_of(&list), vec![boost_id.clone(), reply.clone(), first.clone()], "{list}");

    // Filters.
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses?exclude_replies=true"),
        Some(&alice),
    )
    .await;
    assert_eq!(ids_of(&list), vec![boost_id.clone(), first.clone()], "exclude_replies: {list}");
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses?exclude_reblogs=true"),
        Some(&alice),
    )
    .await;
    assert_eq!(ids_of(&list), vec![reply.clone(), first.clone()], "exclude_reblogs: {list}");

    // Pins: pin first, then second; the newest pin comes first.
    for id in [&first, &reply] {
        let (status, _h, bytes) = send(
            &srv.router,
            form_request("POST", &format!("/api/v1/statuses/{id}/pin"), "", Some(&alice)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    }
    let list = get_ok(
        &srv,
        &format!("/api/v1/accounts/{alice_id}/statuses?pinned=true"),
        Some(&alice),
    )
    .await;
    assert_eq!(ids_of(&list), vec![reply.clone(), first.clone()], "pinned order: {list}");
    for entry in list.as_array().expect("array") {
        assert_eq!(entry["pinned"], true, "pinned flag: {entry}");
    }

    // A full page carries a Link header with rel="next".
    let (status, headers, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/accounts/{alice_id}/statuses?limit=1"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let link = headers
        .get("link")
        .expect("Link header on a full page")
        .to_str()
        .unwrap();
    assert!(link.contains("rel=\"next\""), "Link header: {link}");

    // Unknown account: 404.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/999999999999999999/statuses", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 13. favourited_by / reblogged_by account lists.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn favourited_by_and_reblogged_by_lists() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (alice, _b) = register_account(&srv, &app, "alice").await;
    let (bob, _b) = register_account(&srv, &app, "bob").await;
    let created = create(&srv, &alice, "status=liked+post").await;
    let id = created["id"].as_str().unwrap();

    // Bob favourites and reblogs; the anonymous lists are populated with a
    // Link header (rel="prev" points at the newest entry).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{id}/favourite"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["favourited"], true);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{id}/reblog"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let (status, headers, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{id}/favourited_by"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "optional-auth endpoint: {}", String::from_utf8_lossy(&bytes));
    let favourited_by = json(&bytes);
    assert_eq!(favourited_by.as_array().expect("array").len(), 1, "{favourited_by}");
    assert_eq!(favourited_by[0]["username"], "bob");
    let link = headers
        .get("link")
        .expect("Link header on favourited_by")
        .to_str()
        .unwrap();
    assert!(link.contains("rel=\"prev\""), "Link header: {link}");

    let reblogged_by = get_ok(&srv, &format!("/api/v1/statuses/{id}/reblogged_by"), None).await;
    assert_eq!(reblogged_by.as_array().expect("array").len(), 1, "{reblogged_by}");
    assert_eq!(reblogged_by[0]["username"], "bob");

    // Unfavourite: the list empties out.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{id}/unfavourite"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["favourites_count"], 0);
    let favourited_by = get_ok(&srv, &format!("/api/v1/statuses/{id}/favourited_by"), None).await;
    assert_eq!(favourited_by, serde_json::json!([]), "emptied favourited_by");
}
