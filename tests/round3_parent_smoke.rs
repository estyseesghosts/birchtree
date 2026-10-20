//! Round 3 parent HTTP smoke: the newly wired routes resolve (not 404) and
//! answer their documented shapes. App-layer behavior is covered by the
//! `round3_*` suites; this file only proves the parent router wiring.

mod common;

use axum::http::StatusCode;
use common::{T0, TestServer, form_request, get_request, send};

use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::auth;
use birchtree::application::ports::AccountStore;

/// A user token with exactly `scopes` (direct store issuance).
fn token_with_email(srv: &TestServer, username: &str, email: Option<&str>, scopes: &str) -> String {
    let account = match srv
        .store
        .account_by_handle(username, "example.social")
        .expect("store")
    {
        Some(account) => account,
        None => create_account(
            &*srv.store,
            "https://example.social",
            T0,
            CreateAccountCommand {
                username: username.to_string(),
                email: email.map(str::to_string),
                password: "password123".to_string(),
                display_name: None,
            },
        )
        .expect("create account"),
    };
    let app = auth::register_app(
        &*srv.store,
        format!("Round3 smoke {username}"),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("register app");
    let scopes: Vec<String> = scopes.split_whitespace().map(str::to_string).collect();
    let (plaintext, _) =
        auth::issue_user_token(&*srv.store, app.app.id, account.id, scopes, T0).expect("token");
    plaintext
}

#[tokio::test]
async fn invite_show_unknown_code_is_handler_404() {
    let srv = TestServer::new(true);
    let (status, _, bytes) = send(&srv.router, get_request("/invite/nope-nope", None, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{}", String::from_utf8_lossy(&bytes));
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"], "Record not found");
}

#[tokio::test]
async fn email_confirmation_without_provider_is_explicit_503() {
    let srv = TestServer::new(true);
    // `write` scope covers `write:accounts` via the broad-scope rule.
    let user = token_with_email(&srv, "alice", Some("alice@example.com"), "read write");
    let (status, _, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/emails/confirmations", "", Some(&user)),
    )
    .await;
    // Explicit disabled state (no SMTP configured), never a fake success.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{}", String::from_utf8_lossy(&bytes));
}

#[tokio::test]
async fn notification_policy_and_requests_routes_are_wired() {
    let srv = TestServer::new(true);
    // No token → 401 from the handler, which proves the route exists
    // (an absent route would 404 without reaching auth).
    let (status, _, _) = send(&srv.router, get_request("/api/v1/notifications/policy", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&srv.router, get_request("/api/v1/notifications/requests", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Authenticated policy read works and carries the upstream defaults.
    let user = token_with_email(&srv, "bob", None, "read read:notifications");
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/policy", Some(&user), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["filter_private_mentions"], true);

    // Empty request index is a real empty page, not a stub.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/requests", Some(&user), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body, serde_json::json!([]));
}
