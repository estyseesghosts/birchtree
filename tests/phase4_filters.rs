//! Phase 4 filter contract tests: v2 CRUD + keyword/status rule
//! sub-resources, the v1 API synthesized from v2, and the `filtered` matcher,
//! driven through the real router with the pinned Mastodon v4.7.3 semantics
//! (exact key sets, status codes, error bodies and validation messages).

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;

use birchtree::application::ports::{AccountStore, OAuthStore, StatusStore};

use common::{TestServer, T0, form_request, get_request, send};
use common::json as json_body;

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

/// Every scope the endpoints of this file touch.
const ALL_SCOPES: &str = "read write read:filters write:filters read:statuses \
write:statuses read:accounts write:accounts profile";

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=Phase4+Filter+Tests&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json_body(&bytes)
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
    json_body(&bytes)["access_token"].as_str().unwrap().to_string()
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
    json_body(&bytes)["access_token"].as_str().unwrap().to_string()
}

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
fn account_id(srv: &TestServer, name: &str) -> birchtree::domain::Id {
    srv.store
        .account_by_handle(name, "example.social")
        .unwrap()
        .unwrap()
        .id
}

/// A stored status by its wire id (direct store read).
fn status_of(srv: &TestServer, id: &str) -> birchtree::domain::Status {
    srv.store
        .status_by_id(id.parse().expect("status id"))
        .unwrap()
        .unwrap()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// `POST` form data and return `(status, json body)`.
async fn post(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, token)).await;
    (status, json_body(&bytes))
}

/// `POST` asserting a 200 and returning the body.
async fn post_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, body) = post(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {body}");
    body
}

/// `PATCH` form data and return `(status, json body)`.
async fn patch(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("PATCH", path, body, token)).await;
    (status, json_body(&bytes))
}

/// `PATCH` asserting a 200 and returning the body.
async fn patch_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, body) = patch(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "PATCH {path}: {body}");
    body
}

/// `PUT` form data and return `(status, json body)`.
async fn put(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("PUT", path, body, token)).await;
    (status, json_body(&bytes))
}

/// `PUT` asserting a 200 and returning the body.
async fn put_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, body) = put(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "PUT {path}: {body}");
    body
}

/// `DELETE` asserting a 200 and returning the body.
async fn delete_ok(srv: &TestServer, path: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("DELETE", path, "", token)).await;
    assert_eq!(status, StatusCode::OK, "DELETE {path}: {}", String::from_utf8_lossy(&bytes));
    json_body(&bytes)
}

/// `GET` asserting a 200 and returning the body.
async fn get_ok(srv: &TestServer, path: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) = send(&srv.router, get_request(path, token, None)).await;
    assert_eq!(status, StatusCode::OK, "GET {path}: {}", String::from_utf8_lossy(&bytes));
    json_body(&bytes)
}

/// `GET` returning `(status, json body)`.
async fn get(srv: &TestServer, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(&srv.router, get_request(path, token, None)).await;
    (status, json_body(&bytes))
}

/// The sorted key set of a JSON object.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected object, got {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_keys(value: &Value, expected: &[&str]) {
    let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(keys(value), expected, "keys of {value}");
}

fn assert_error(status: StatusCode, body: &Value, expected_status: StatusCode, message: &str) {
    assert_eq!(status, expected_status, "body: {body}");
    assert_eq!(body, &json!({ "error": message }));
}

// ---------------------------------------------------------------------------
// 1. v2 CRUD.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v2_create_returns_exact_key_set_and_round_trips() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let created = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Noise&context[]=home&context[]=public&expires_in=3600&filter_action=warn\
&keywords_attributes[0][keyword]=spoiler&keywords_attributes[0][whole_word]=1\
&keywords_attributes[1][keyword]=vague",
    )
    .await;
    assert_keys(
        &created,
        &["id", "title", "context", "expires_at", "filter_action", "keywords", "statuses"],
    );
    assert_eq!(created["title"], "Noise");
    assert_eq!(created["context"], json!(["home", "public"]));
    assert!(created["expires_at"].is_string(), "{}", created["expires_at"]);
    assert_eq!(created["filter_action"], "warn");
    assert_eq!(created["statuses"], json!([]));
    let keywords = created["keywords"].as_array().unwrap();
    assert_eq!(keywords.len(), 2);
    assert_keys(&keywords[0], &["id", "keyword", "whole_word"]);
    assert_eq!(keywords[0]["keyword"], "spoiler");
    assert_eq!(keywords[0]["whole_word"], true);
    assert_eq!(keywords[1]["keyword"], "vague");
    assert_eq!(keywords[1]["whole_word"], false);

    // Index and show round-trip the exact object.
    let index = get_ok(&srv, "/api/v2/filters", Some(alice)).await;
    assert_eq!(index, json!([created.clone()]));
    let id = created["id"].as_str().unwrap();
    let show = get_ok(&srv, &format!("/api/v2/filters/{id}"), Some(alice)).await;
    assert_eq!(show, created);
}

#[tokio::test]
async fn v2_update_patches_fields_clears_expiry_and_replaces_keywords() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let created = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Old&context[]=home&expires_in=3600\
&keywords_attributes[0][keyword]=alpha",
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let alpha = created["keywords"][0]["id"].as_str().unwrap().to_string();

    // Title, action and context in one patch.
    let updated = patch_ok(
        &srv,
        &format!("/api/v2/filters/{id}"),
        Some(alice),
        "title=New&context[]=notifications&filter_action=hide",
    )
    .await;
    assert_eq!(updated["title"], "New");
    assert_eq!(updated["context"], json!(["notifications"]));
    assert_eq!(updated["filter_action"], "hide");
    assert!(updated["expires_at"].is_string(), "expiry unchanged");

    // `expires_in=0` clears the expiry (Mastodon never semantics).
    let cleared = patch_ok(
        &srv,
        &format!("/api/v2/filters/{id}"),
        Some(alice),
        "expires_in=0",
    )
    .await;
    assert_eq!(cleared["expires_at"], Value::Null);
    assert_eq!(cleared["title"], "New", "other fields unchanged");

    // `keywords_attributes`: update by id, append without id, then destroy.
    let with_keywords = patch_ok(
        &srv,
        &format!("/api/v2/filters/{id}"),
        Some(alice),
        &format!(
            "keywords_attributes[0][id]={alpha}&keywords_attributes[0][keyword]=renamed\
&keywords_attributes[0][whole_word]=1&keywords_attributes[1][keyword]=added"
        ),
    )
    .await;
    assert_eq!(with_keywords, get_ok(&srv, &format!("/api/v2/filters/{id}"), Some(alice)).await);
    let keywords = with_keywords["keywords"].as_array().unwrap();
    assert_eq!(keywords.len(), 2);
    assert_eq!(keywords[0]["id"], alpha);
    assert_eq!(keywords[0]["keyword"], "renamed");
    assert_eq!(keywords[0]["whole_word"], true);
    assert_eq!(keywords[1]["keyword"], "added");
    let added = keywords[1]["id"].as_str().unwrap().to_string();

    let destroyed = patch_ok(
        &srv,
        &format!("/api/v2/filters/{id}"),
        Some(alice),
        &format!("keywords_attributes[0][id]={added}&keywords_attributes[0][_destroy]=1"),
    )
    .await;
    let keywords = destroyed["keywords"].as_array().unwrap();
    assert_eq!(keywords.len(), 1);
    assert_eq!(keywords[0]["keyword"], "renamed");

    // Delete: `200 {}` and the filter is gone.
    let body = delete_ok(&srv, &format!("/api/v2/filters/{id}"), Some(alice)).await;
    assert_eq!(body, json!({}));
    let (status, body) = get(&srv, &format!("/api/v2/filters/{id}"), Some(alice)).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "Record not found");
}

#[tokio::test]
async fn v2_create_validation_messages_are_exact() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    // Title blank (explicitly or by absence).
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=&context[]=home",
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Title can't be blank" })
    );
    let (_, body) = post(&srv, "/api/v2/filters", Some(alice), "context[]=home").await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Title can't be blank" })
    );

    // Title too long (257 characters).
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        &format!("title={}&context[]=home", "x".repeat(257)),
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Title is too long (maximum is 256 characters)" })
    );

    // Context missing, empty or invalid.
    let (_, body) = post(&srv, "/api/v2/filters", Some(alice), "title=ok").await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Context is not included in the list" })
    );
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=ok&context[]=bogus",
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Context is not included in the list" })
    );

    // Bad filter_action.
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=ok&context[]=home&filter_action=obliterate",
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Action is not included in the list" })
    );

    // Bad expires_in.
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=ok&context[]=home&expires_in=soon",
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Expires in is not a number" })
    );

    // Blank / over-long keyword.
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=ok&context[]=home&keywords_attributes[0][keyword]=",
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Keyword can't be blank" })
    );
    let (_, body) = post(
        &srv,
        "/api/v2/filters",
        Some(alice),
        &format!(
            "title=ok&context[]=home&keywords_attributes[0][keyword]={}",
            "k".repeat(501)
        ),
    )
    .await;
    assert_eq!(
        body,
        json!({ "error": "Validation failed: Keyword is too long (maximum is 500 characters)" })
    );
}

// ---------------------------------------------------------------------------
// 2. Keyword and status rule sub-resources.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keyword_subresource_crud_with_exact_key_sets() {
    let (srv, tokens) = users(&["alice", "bob"]).await;
    let alice = tok(&tokens, "alice");
    let bob = tok(&tokens, "bob");

    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Rules&context[]=home",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();

    // Create.
    let keyword = post_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(alice),
        "keyword=foo&whole_word=true",
    )
    .await;
    assert_keys(&keyword, &["id", "keyword", "whole_word"]);
    assert_eq!(keyword["keyword"], "foo");
    assert_eq!(keyword["whole_word"], true);
    let keyword_id = keyword["id"].as_str().unwrap().to_string();

    // Blank keyword -> 422.
    let (status, body) = post(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(alice),
        "keyword=",
    )
    .await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Validation failed: Keyword can't be blank",
    );

    // Index + show.
    let index = get_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(alice),
    )
    .await;
    assert_eq!(index, json!([keyword.clone()]));
    let show = get_ok(&srv, &format!("/api/v2/filters/keywords/{keyword_id}"), Some(alice)).await;
    assert_eq!(show, keyword);

    // Update.
    let updated = patch_ok(
        &srv,
        &format!("/api/v2/filters/keywords/{keyword_id}"),
        Some(alice),
        "keyword=bar&whole_word=0",
    )
    .await;
    assert_eq!(updated["id"], keyword_id);
    assert_eq!(updated["keyword"], "bar");
    assert_eq!(updated["whole_word"], false);

    // Delete returns the deleted keyword.
    let deleted = delete_ok(&srv, &format!("/api/v2/filters/keywords/{keyword_id}"), Some(alice)).await;
    assert_eq!(deleted, updated);
    let index = get_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(alice),
    )
    .await;
    assert_eq!(index, json!([]));

    // Foreign access is 404 everywhere.
    let (status, body) = get(&srv, &format!("/api/v2/filters/keywords/{keyword_id}"), Some(bob)).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "Record not found");
    let (status, body) = post(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(bob),
        "keyword=intruder",
    )
    .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "Record not found");
}

#[tokio::test]
async fn status_subresource_adds_and_removes_rules() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let status = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=hello+world").await;
    let status_id = status["id"].as_str().unwrap().to_string();
    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Rules&context[]=home",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();

    // Missing status_id -> 400 (param required).
    let (status_code, body) = post(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        "",
    )
    .await;
    assert_error(
        status_code,
        &body,
        StatusCode::BAD_REQUEST,
        "param is missing or the value is empty: status_id",
    );

    // Create.
    let rule = post_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        &format!("status_id={status_id}"),
    )
    .await;
    assert_keys(&rule, &["id", "status_id"]);
    assert_eq!(rule["status_id"], status_id);
    let rule_id = rule["id"].as_str().unwrap().to_string();

    // Unknown status_id -> 404.
    let (status_code, body) = post(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        "status_id=999999999",
    )
    .await;
    assert_error(status_code, &body, StatusCode::NOT_FOUND, "Record not found");

    // Index + show + destroy.
    let index = get_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
    )
    .await;
    assert_eq!(index, json!([rule.clone()]));
    let show = get_ok(&srv, &format!("/api/v2/filters/statuses/{rule_id}"), Some(alice)).await;
    assert_eq!(show, rule);
    let deleted = delete_ok(&srv, &format!("/api/v2/filters/statuses/{rule_id}"), Some(alice)).await;
    assert_eq!(deleted, rule);
    let index = get_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
    )
    .await;
    assert_eq!(index, json!([]));
}

// ---------------------------------------------------------------------------
// 3. v1 (synthesized from v2).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v1_index_projects_keyword_rows() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let status = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=keep+this").await;
    let status_id = status["id"].as_str().unwrap().to_string();
    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Two&context[]=home&context[]=notifications&expires_in=3600\
&keywords_attributes[0][keyword]=alpha&keywords_attributes[0][whole_word]=1\
&keywords_attributes[1][keyword]=beta",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap();
    let alpha_id = filter["keywords"][0]["id"].as_str().unwrap().to_string();
    let beta_id = filter["keywords"][1]["id"].as_str().unwrap().to_string();
    post_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        &format!("status_id={status_id}"),
    )
    .await;

    // One v1 row per keyword; the id is the keyword id.
    let rows = get_ok(&srv, "/api/v1/filters", Some(alice)).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_keys(
            row,
            &["id", "phrase", "context", "whole_word", "expires_at", "irreversible", "status_ids"],
        );
        assert_eq!(row["context"], json!(["home", "notifications"]));
        assert!(row["expires_at"].is_string());
        assert_eq!(row["irreversible"], false);
        assert_eq!(row["status_ids"], json!([status_id.clone()]));
    }
    let alpha = rows.iter().find(|row| row["id"] == alpha_id).expect("alpha row");
    assert_eq!(alpha["phrase"], "alpha");
    assert_eq!(alpha["whole_word"], true);
    let beta = rows.iter().find(|row| row["id"] == beta_id).expect("beta row");
    assert_eq!(beta["phrase"], "beta");
    assert_eq!(beta["whole_word"], false);
}

#[tokio::test]
async fn v1_create_makes_filter_and_keyword_atomically() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    // Missing phrase -> 400 (param required).
    let (status, body) = post(&srv, "/api/v1/filters", Some(alice), "context[]=home").await;
    assert_error(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "param is missing or the value is empty: phrase",
    );

    // Blank phrase -> title blank 422 (the filter row validates first).
    let (status, body) = post(&srv, "/api/v1/filters", Some(alice), "phrase=&context[]=home").await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Validation failed: Title can't be blank",
    );

    // Missing context -> 422.
    let (status, body) = post(&srv, "/api/v1/filters", Some(alice), "phrase=foo").await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Validation failed: Context is not included in the list",
    );

    // One call creates filter + keyword.
    let row = post_ok(
        &srv,
        "/api/v1/filters",
        Some(alice),
        "phrase=foo&context[]=home&irreversible=true&whole_word=true",
    )
    .await;
    assert_keys(
        &row,
        &["id", "phrase", "context", "whole_word", "expires_at", "irreversible", "status_ids"],
    );
    assert_eq!(row["phrase"], "foo");
    assert_eq!(row["context"], json!(["home"]));
    assert_eq!(row["whole_word"], true);
    assert_eq!(row["irreversible"], true);
    assert_eq!(row["expires_at"], Value::Null);
    assert_eq!(row["status_ids"], json!([]));

    // The v2 view: filter with title=foo, action=hide, one keyword.
    let v2 = get_ok(&srv, "/api/v2/filters", Some(alice)).await;
    let v2 = v2.as_array().unwrap();
    assert_eq!(v2.len(), 1);
    assert_eq!(v2[0]["title"], "foo");
    assert_eq!(v2[0]["filter_action"], "hide");
    assert_eq!(v2[0]["keywords"].as_array().unwrap().len(), 1);
    assert_eq!(v2[0]["keywords"][0]["keyword"], "foo");
    assert_eq!(v2[0]["keywords"][0]["id"], row["id"]);
}

#[tokio::test]
async fn v1_update_patches_single_keyword_filters() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let row = post_ok(
        &srv,
        "/api/v1/filters",
        Some(alice),
        "phrase=foo&context[]=home&whole_word=1",
    )
    .await;
    let keyword_id = row["id"].as_str().unwrap().to_string();
    let filter_id = get_ok(&srv, "/api/v2/filters", Some(alice)).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = put_ok(
        &srv,
        &format!("/api/v1/filters/{keyword_id}"),
        Some(alice),
        "phrase=bar&context[]=public&whole_word=0&expires_in=7200&irreversible=false",
    )
    .await;
    assert_eq!(updated["id"], keyword_id);
    assert_eq!(updated["phrase"], "bar");
    assert_eq!(updated["context"], json!(["public"]));
    assert_eq!(updated["whole_word"], false);
    assert!(updated["expires_at"].is_string());
    assert_eq!(updated["irreversible"], false);

    // The v2 view reflects the parent changes (title is a v2-only field).
    let filter = get_ok(&srv, &format!("/api/v2/filters/{filter_id}"), Some(alice)).await;
    assert_eq!(filter["title"], "foo");
    assert_eq!(filter["context"], json!(["public"]));
    assert_eq!(filter["filter_action"], "warn");
    assert!(filter["expires_at"].is_string());
    assert_eq!(filter["keywords"][0]["keyword"], "bar");
    assert_eq!(filter["keywords"][0]["whole_word"], false);

    // `expires_in=0` clears the expiry again.
    let updated = put_ok(
        &srv,
        &format!("/api/v1/filters/{keyword_id}"),
        Some(alice),
        "expires_in=0",
    )
    .await;
    assert_eq!(updated["expires_at"], Value::Null);
}

#[tokio::test]
async fn v1_update_multi_keyword_guard() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Two&context[]=home\
&keywords_attributes[0][keyword]=alpha&keywords_attributes[1][keyword]=beta",
    )
    .await;
    let alpha_id = filter["keywords"][0]["id"].as_str().unwrap().to_string();
    const GUARD: &str = "These parameters cannot be changed from this application because they \
apply to more than one filter keyword. Use a more recent application or the web interface.";

    // Parent attributes are pinned for multi-keyword filters.
    for body in ["irreversible=1", "context[]=public", "expires_in=60"] {
        let (status, body) = put(
            &srv,
            &format!("/api/v1/filters/{alpha_id}"),
            Some(alice),
            body,
        )
        .await;
        assert_error(status, &body, StatusCode::UNPROCESSABLE_ENTITY, GUARD);
    }

    // Keyword-level changes still go through.
    let updated = put_ok(
        &srv,
        &format!("/api/v1/filters/{alpha_id}"),
        Some(alice),
        "phrase=omega",
    )
    .await;
    assert_eq!(updated["phrase"], "omega");
    let filter_id = filter["id"].as_str().unwrap();
    let filter = get_ok(&srv, &format!("/api/v2/filters/{filter_id}"), Some(alice)).await;
    let keywords = filter["keywords"].as_array().unwrap();
    assert_eq!(keywords.len(), 2);
    assert_eq!(keywords[0]["keyword"], "omega");
    assert_eq!(keywords[1]["keyword"], "beta");
}

#[tokio::test]
async fn v1_destroy_removes_only_the_keyword() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");

    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Two&context[]=home\
&keywords_attributes[0][keyword]=alpha&keywords_attributes[1][keyword]=beta",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();
    let alpha_id = filter["keywords"][0]["id"].as_str().unwrap().to_string();

    let body = delete_ok(&srv, &format!("/api/v1/filters/{alpha_id}"), Some(alice)).await;
    assert_eq!(body, json!({}));

    // The keyword is gone; the parent filter (and its other keyword) stays.
    let rows = get_ok(&srv, "/api/v1/filters", Some(alice)).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["phrase"], "beta");
    let filter = get_ok(&srv, &format!("/api/v2/filters/{filter_id}"), Some(alice)).await;
    let keywords = filter["keywords"].as_array().unwrap();
    assert_eq!(keywords.len(), 1);
    assert_eq!(keywords[0]["keyword"], "beta");
}

// ---------------------------------------------------------------------------
// 4. Ownership and scopes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn foreign_filters_answer_record_not_found() {
    let (srv, tokens) = users(&["alice", "bob"]).await;
    let alice = tok(&tokens, "alice");
    let bob = tok(&tokens, "bob");

    let status = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=mine").await;
    let status_id = status["id"].as_str().unwrap().to_string();
    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=Mine&context[]=home&keywords_attributes[0][keyword]=alpha",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();
    let keyword_id = filter["keywords"][0]["id"].as_str().unwrap().to_string();
    let rule = post_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        &format!("status_id={status_id}"),
    )
    .await;
    let rule_id = rule["id"].as_str().unwrap().to_string();

    const NOT_FOUND: &str = "Record not found";
    // GET/PUT|PATCH/DELETE on every foreign id -> 404 with the exact body.
    let mut checks: Vec<(String, &'static str, String)> = Vec::new();
    for path in [
        format!("/api/v2/filters/{filter_id}"),
        format!("/api/v2/filters/keywords/{keyword_id}"),
        format!("/api/v2/filters/statuses/{rule_id}"),
        format!("/api/v1/filters/{keyword_id}"),
    ] {
        checks.push((path.clone(), "GET", String::new()));
    }
    for path in [
        format!("/api/v2/filters/{filter_id}"),
        format!("/api/v2/filters/keywords/{keyword_id}"),
    ] {
        checks.push((path.clone(), "PATCH", "keyword=x".to_string()));
    }
    for path in [
        format!("/api/v1/filters/{keyword_id}"),
        format!("/api/v2/filters/{filter_id}"),
        format!("/api/v2/filters/keywords/{keyword_id}"),
        format!("/api/v2/filters/statuses/{rule_id}"),
    ] {
        checks.push((path, "DELETE", String::new()));
    }
    for (path, method, body) in checks {
        let (status_code, _h, bytes) = send(
            &srv.router,
            form_request(method, &path, &body, Some(bob)),
        )
        .await;
        assert_eq!(status_code, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(json_body(&bytes), serde_json::json!({ "error": NOT_FOUND }));
    }

    // Writes through a foreign parent.
    for (path, body) in [
        (
            format!("/api/v2/filters/{filter_id}/keywords"),
            "keyword=x".to_string(),
        ),
        (
            format!("/api/v2/filters/{filter_id}/statuses"),
            format!("status_id={status_id}"),
        ),
    ] {
        let (status_code, body) = post(&srv, &path, Some(bob), &body).await;
        assert_error(status_code, &body, StatusCode::NOT_FOUND, NOT_FOUND);
    }
}

#[tokio::test]
async fn wrong_scope_is_forbidden() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");
    let filter = post_ok(
        &srv,
        "/api/v2/filters",
        Some(alice),
        "title=One&context[]=home&keywords_attributes[0][keyword]=alpha",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();
    let keyword_id = filter["keywords"][0]["id"].as_str().unwrap().to_string();

    // Mint narrowly scoped tokens for the same account.
    let app = srv
        .store
        .app_by_client_id(
            register_app(&srv, ALL_SCOPES).await["client_id"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    let account = srv.store.account_by_handle("alice", "example.social").unwrap().unwrap();
    let mut scoped = HashMap::new();
    for scope in ["read", "write", "read:filters", "write:filters"] {
        let (token, _record) = birchtree::application::auth::issue_user_token(
            &*srv.store,
            app.id,
            account.id,
            vec![scope.to_string()],
            T0,
        )
        .unwrap();
        scoped.insert(scope.to_string(), token);
    }

    const OUTSIDE: &str = "This action is outside the authorized scopes";
    // Reads require read/read:filters.
    let (status, body) = get(&srv, "/api/v2/filters", Some(&scoped["write"])).await;
    assert_error(status, &body, StatusCode::FORBIDDEN, OUTSIDE);
    let (status, _body) = get(&srv, "/api/v2/filters", Some(&scoped["read"])).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _body) = get(&srv, "/api/v2/filters", Some(&scoped["read:filters"])).await;
    assert_eq!(status, StatusCode::OK);
    // Writes require write/write:filters.
    let (status, body) = post(
        &srv,
        "/api/v2/filters",
        Some(&scoped["read"]),
        "title=one&context[]=home",
    )
    .await;
    assert_error(status, &body, StatusCode::FORBIDDEN, OUTSIDE);
    let (status, body) = patch(
        &srv,
        &format!("/api/v2/filters/keywords/{keyword_id}"),
        Some(&scoped["read"]),
        "keyword=x",
    )
    .await;
    assert_error(status, &body, StatusCode::FORBIDDEN, OUTSIDE);
    // The broad `write` scope is sufficient for writes (and refused for reads).
    let (status, _body) = post(
        &srv,
        "/api/v2/filters",
        Some(&scoped["write"]),
        "title=two&context[]=home",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _body) = post(
        &srv,
        &format!("/api/v2/filters/{filter_id}/keywords"),
        Some(&scoped["write:filters"]),
        "keyword=ok",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 5. The `filtered` matcher.
// ---------------------------------------------------------------------------

/// Build one filter for a viewer through the API and return its id.
async fn make_filter(srv: &TestServer, token: &str, body: &str) -> String {
    post_ok(srv, "/api/v2/filters", Some(token), body)
        .await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn filter_results_matches_keywords_with_exact_payload() {
    let (srv, tokens) = users(&["alice"]).await;
    let alice = tok(&tokens, "alice");
    let alice_id = account_id(&srv, "alice");

    let filter_id = make_filter(
        &srv,
        alice,
        "title=Fan+Club&context[]=home&keywords_attributes[0][keyword]=Rustacean",
    )
    .await;

    let matching =
        post_ok(&srv, "/api/v1/statuses", Some(alice), "status=rustaceans+are+great").await;
    let plain = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=nothing+here").await;
    let statuses = vec![
        status_of(&srv, matching["id"].as_str().unwrap()),
        status_of(&srv, plain["id"].as_str().unwrap()),
    ];

    let results =
        birchtree::application::filters::filter_results(&*srv.store, alice_id, &statuses, "home", now_ms())
            .unwrap();

    // Only the matching status carries entries.
    assert_eq!(results.len(), 1);
    let matching_id: birchtree::domain::Id = matching["id"].as_str().unwrap().parse().unwrap();
    let entries = results.get(&matching_id).expect("matches for the status");
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_keys(entry, &["filter", "keyword_matches", "status_matches"]);
    assert_eq!(entry["keyword_matches"], json!(["Rustacean"]));
    assert_eq!(entry["status_matches"], json!([]));
    // The embedded filter is the no-rules projection.
    let filter = &entry["filter"];
    assert_keys(filter, &["id", "title", "context", "expires_at", "filter_action"]);
    assert_eq!(filter["id"], filter_id);
    assert_eq!(filter["title"], "Fan Club");
    assert_eq!(filter["context"], json!(["home"]));
    assert_eq!(filter["filter_action"], "warn");
    assert_eq!(filter["expires_at"], Value::Null);
}

#[tokio::test]
async fn filter_results_whole_word_expiry_and_context() {
    let (srv, tokens) = users(&["whole", "expiry", "narrow"]).await;

    // whole_word: "art" matches "art show" but NOT "cart".
    let whole_tok = tok(&tokens, "whole");
    let whole_id = account_id(&srv, "whole");
    make_filter(
        &srv,
        whole_tok,
        "title=Art&context[]=home&keywords_attributes[0][keyword]=art\
&keywords_attributes[0][whole_word]=1",
    )
    .await;
    let cart = post_ok(&srv, "/api/v1/statuses", Some(whole_tok), "status=a+cart").await;
    let art = post_ok(&srv, "/api/v1/statuses", Some(whole_tok), "status=an+art+show").await;
    let statuses = vec![
        status_of(&srv, cart["id"].as_str().unwrap()),
        status_of(&srv, art["id"].as_str().unwrap()),
    ];
    let results =
        birchtree::application::filters::filter_results(&*srv.store, whole_id, &statuses, "home", now_ms())
            .unwrap();
    let art_id: birchtree::domain::Id = art["id"].as_str().unwrap().parse().unwrap();
    let cart_id: birchtree::domain::Id = cart["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(results.len(), 1, "only the whole-word match");
    assert!(results.contains_key(&art_id));
    assert!(!results.contains_key(&cart_id));

    // Expiry: an expired filter matches nothing.
    let expiry_tok = tok(&tokens, "expiry");
    let expiry_id = account_id(&srv, "expiry");
    make_filter(
        &srv,
        expiry_tok,
        "title=Temp&context[]=home&expires_in=1\
&keywords_attributes[0][keyword]=evergreen",
    )
    .await;
    let post = post_ok(&srv, "/api/v1/statuses", Some(expiry_tok), "status=evergreen+topic").await;
    let status = status_of(&srv, post["id"].as_str().unwrap());
    let now = now_ms();
    let live =
        birchtree::application::filters::filter_results(&*srv.store, expiry_id, std::slice::from_ref(&status), "home", now)
            .unwrap();
    assert_eq!(live.len(), 1, "fresh filter still matches");
    let expired = birchtree::application::filters::filter_results(
        &*srv.store,
        expiry_id,
        std::slice::from_ref(&status),
        "home",
        now + 10_000,
    )
    .unwrap();
    assert!(expired.is_empty(), "expired filter matches nothing");

    // Context narrowing: a home filter does not match for "public".
    let narrow_tok = tok(&tokens, "narrow");
    let narrow_id = account_id(&srv, "narrow");
    make_filter(
        &srv,
        narrow_tok,
        "title=Home+Only&context[]=home&keywords_attributes[0][keyword]=contextkw",
    )
    .await;
    let post = post_ok(&srv, "/api/v1/statuses", Some(narrow_tok), "status=contextkw+text").await;
    let status = status_of(&srv, post["id"].as_str().unwrap());
    let now = now_ms();
    for context in ["public", "notifications", "account_list"] {
        let results = birchtree::application::filters::filter_results(
            &*srv.store,
            narrow_id,
            std::slice::from_ref(&status),
            context,
            now,
        )
        .unwrap();
        assert!(results.is_empty(), "no match for context {context}");
    }
    let results =
        birchtree::application::filters::filter_results(&*srv.store, narrow_id, std::slice::from_ref(&status), "home", now)
            .unwrap();
    assert_eq!(results.len(), 1, "matches in its own context");
}

#[tokio::test]
async fn filter_results_status_rules_polls_and_reblogs() {
    let (srv, tokens) = users(&["alice", "bob"]).await;
    let alice = tok(&tokens, "alice");
    let bob = tok(&tokens, "bob");
    let alice_id = account_id(&srv, "alice");

    // A keyword rule that never matches + a status rule on alice's post.
    let plain = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=boring+words").await;
    let plain_id = plain["id"].as_str().unwrap().to_string();
    let filter_id = make_filter(
        &srv,
        alice,
        "title=Rules&context[]=home&keywords_attributes[0][keyword]=nevermatch123",
    )
    .await;
    post_ok(
        &srv,
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(alice),
        &format!("status_id={plain_id}"),
    )
    .await;

    // The status-id rule matches with empty keyword_matches.
    let status = status_of(&srv, &plain_id);
    let results =
        birchtree::application::filters::filter_results(&*srv.store, alice_id, std::slice::from_ref(&status), "home", now_ms())
            .unwrap();
    let plain_pk: birchtree::domain::Id = plain_id.parse().unwrap();
    let entries = results.get(&plain_pk).expect("status rule match");
    assert_eq!(entries[0]["keyword_matches"], json!([]));
    assert_eq!(entries[0]["status_matches"], json!([plain_id.clone()]));

    // Bob reblogs the status: matches land under the wrapper AND the origin.
    let reblog = post_ok(
        &srv,
        &format!("/api/v1/statuses/{plain_id}/reblog"),
        Some(bob),
        "",
    )
    .await;
    let reblog_id: birchtree::domain::Id = reblog["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(reblog_id, plain_pk, "the reblog wrapper has its own id");
    let reblog_status = status_of(&srv, reblog["id"].as_str().unwrap());
    let results =
        birchtree::application::filters::filter_results(&*srv.store, alice_id, std::slice::from_ref(&reblog_status), "home", now_ms())
            .unwrap();
    assert_eq!(results.len(), 2, "wrapper + origin ids");
    assert_eq!(
        results.get(&reblog_id).unwrap()[0]["status_matches"],
        json!([plain_id.clone()])
    );
    assert_eq!(
        results.get(&plain_pk).unwrap()[0]["status_matches"],
        json!([plain_id.clone()])
    );

    // Poll option titles are searched too.
    let poll = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=vote&poll[options][]=kiwi&poll[options][]=banana&poll[expires_in]=3600",
    )
    .await;
    make_filter(
        &srv,
        alice,
        "title=Fruit&context[]=home&keywords_attributes[0][keyword]=banana",
    )
    .await;
    let poll_status = status_of(&srv, poll["id"].as_str().unwrap());
    let results =
        birchtree::application::filters::filter_results(&*srv.store, alice_id, std::slice::from_ref(&poll_status), "home", now_ms())
            .unwrap();
    let poll_pk: birchtree::domain::Id = poll["id"].as_str().unwrap().parse().unwrap();
    let entries = results.get(&poll_pk).expect("poll option match");
    assert_eq!(entries[0]["keyword_matches"], json!(["banana"]));
}
