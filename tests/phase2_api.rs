//! Phase 2 API contract tests: account registration, instance, account read /
//! update, preferences and relationships.

mod common;

use axum::http::StatusCode;
use birchtree::application::ports::AccountStore;

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

const ALL_SCOPES: &str = "read write profile read:accounts write:accounts";

#[tokio::test]
async fn registration_returns_user_token() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (_token, body) = register_account(&srv, &app, "newuser").await;

    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["scope"], ALL_SCOPES);
    assert!(body["access_token"].as_str().unwrap().len() == 64);
    assert!(body["created_at"].is_number());
}

#[tokio::test]
async fn registration_closed_returns_forbidden() {
    let srv = TestServer::new(false);
    let app = register_app(&srv, ALL_SCOPES).await;
    let app = app_token(&srv, &app, "write:accounts").await;
    let body = format!(
        "username=blocked&email={}&password=password123&agreement=true",
        enc("blocked@example.com"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "Forbidden");
}

#[tokio::test]
async fn registration_needs_client_credentials() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (user_token, _body) = register_account(&srv, &app, "firstuser").await;

    let body = format!(
        "username=second&email={}&password=password123&agreement=true",
        enc("second@example.com"),
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&user_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        json(&bytes)["error"],
        "This method requires an client credentials authentication"
    );
}

#[tokio::test]
async fn registration_needs_write_scope() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let token = app_token(&srv, &app, "read").await;
    let body = format!(
        "username=noscape&email={}&password=password123&agreement=true",
        enc("noscape@example.com"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&token))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        json(&bytes)["error"],
        "This action is outside the authorized scopes"
    );
}

#[tokio::test]
async fn duplicate_username_is_rejected() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (_token, _body) = register_account(&srv, &app, "dupe").await;

    let token = app_token(&srv, &app, "write:accounts").await;
    let body = format!(
        "username=dupe&email={}&password=password123&agreement=true",
        enc("dupe2@example.com"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&token))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = json(&bytes);
    assert_eq!(body["details"]["username"][0]["error"], "ERR_TAKEN");
}

#[tokio::test]
async fn verify_credentials_returns_credential_shape() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _body) = register_account(&srv, &app, "creduser").await;

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let account = json(&bytes);
    assert_eq!(account["username"], "creduser");
    assert_eq!(account["acct"], "creduser");
    assert_eq!(account["source"]["privacy"], "public");
    assert_eq!(account["source"]["quote_policy"], "public");
    assert_eq!(account["role"]["name"], "User");
    assert!(account["created_at"].is_string());
    assert_eq!(account["followers_count"], 0);
}

#[tokio::test]
async fn account_show_is_public_and_404s_unknown() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (_token, _body) = register_account(&srv, &app, "publicuser").await;

    let account = srv
        .store
        .account_by_handle("publicuser", "example.social")
        .unwrap()
        .unwrap();

    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/accounts/{}", account.id), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["id"], account.id.to_string());

    let (status, _h, _b) = send(
        &srv.router,
        get_request("/api/v1/accounts/99999999", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _h, _b) = send(
        &srv.router,
        get_request("/api/v1/accounts/not-a-number", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_credentials_persists_profile_and_settings() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _body) = register_account(&srv, &app, "editor").await;

    let body = "display_name=New+Name&note=Hello+there&locked=true&\
                source[privacy]=private&source[sensitive]=true&source[language]=fr&\
                source[quote_policy]=followers";
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            body,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let account = json(&bytes);
    assert_eq!(account["display_name"], "New Name");
    assert_eq!(account["note"], "Hello there");
    assert_eq!(account["locked"], true);
    assert_eq!(account["source"]["privacy"], "private");
    assert_eq!(account["source"]["sensitive"], true);
    assert_eq!(account["source"]["language"], "fr");
    assert_eq!(account["source"]["quote_policy"], "followers");

    // Preferences reflect the same settings.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/preferences", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let prefs = json(&bytes);
    assert_eq!(prefs["posting:default:visibility"], "private");
    assert_eq!(prefs["posting:default:sensitive"], true);
    assert_eq!(prefs["posting:default:language"], "fr");
    assert_eq!(prefs["posting:default:quote_policy"], "followers");
    assert_eq!(prefs["reading:autoplay:gifs"], false);
}

#[tokio::test]
async fn preferences_require_a_user() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let app_token = app_token(&srv, &app, "read").await;
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/preferences", Some(&app_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "This method requires an authenticated user"
    );
}

#[tokio::test]
async fn missing_token_is_unauthorized() {
    let srv = TestServer::new(true);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/preferences", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&bytes)["error"], "The access token is invalid");
}

#[tokio::test]
async fn relationships_returns_relationship_shape() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _body) = register_account(&srv, &app, "reluser").await;
    let other = common::mk_account(&srv.store, "other");

    let uri = format!("/api/v1/accounts/relationships?id[]={}", other.id);
    let (status, _h, bytes) = send(&srv.router, get_request(&uri, Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let relationships = json(&bytes);
    assert!(relationships.is_array());
    assert_eq!(relationships[0]["id"], other.id.to_string());
    assert_eq!(relationships[0]["following"], false);
    assert_eq!(relationships[0]["note"], "");
}

#[tokio::test]
async fn instance_v1_shape() {
    let srv = TestServer::new(true);
    let (status, _h, bytes) = send(&srv.router, get_request("/api/v1/instance", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let instance = json(&bytes);
    assert_eq!(instance["uri"], "example.social");
    assert_eq!(instance["title"], "Birchtree");
    assert_eq!(instance["registrations"], true);
    assert_eq!(instance["version"], "4.7.3 (compatible; birchtree 0.1.0)");
    assert!(instance["stats"]["user_count"].is_number());
    assert!(instance["urls"]["streaming_api"].is_string());
    assert!(instance["configuration"]["statuses"]["max_characters"].is_number());
    assert_eq!(instance["contact_account"], serde_json::Value::Null);
}

#[tokio::test]
async fn instance_v2_shape_has_no_top_level_stats() {
    let srv = TestServer::new(false);
    let (status, _h, bytes) = send(&srv.router, get_request("/api/v2/instance", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let instance = json(&bytes);
    assert_eq!(instance["domain"], "example.social");
    assert_eq!(instance["registrations"]["enabled"], false);
    assert_eq!(instance["configuration"]["limited_federation"], false);
    assert!(instance.get("stats").is_none());
    assert!(instance.get("urls").is_none());
    assert!(instance["usage"]["users"]["active_month"].is_number());
    assert!(instance["contact"]["email"].is_string());
}
