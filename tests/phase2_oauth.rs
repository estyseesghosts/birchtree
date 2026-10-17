//! Phase 2 OAuth contract tests: app registration, client-credentials,
//! authorization-code + PKCE, revoke, and scope/credential enforcement.
//!
//! These exercise the real router in-process (no network).

mod common;

use axum::http::StatusCode;
use birchtree::application::crypto;

use common::{TestServer, form_request, get_request, json, send};

/// Percent-encode a form component (RFC 3986 unreserved plus `+` not used).
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

/// Extract a query parameter from a URL (path or absolute).
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Register an application with the given scopes and return its JSON body.
async fn register_app(srv: &TestServer, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _headers, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "register app: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// Obtain an app-only token via the `client_credentials` grant.
async fn client_credentials_token(
    srv: &TestServer,
    app: &serde_json::Value,
    scopes: &str,
) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _headers, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "token: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// Run the browser authorization-code + PKCE flow and return a user token.
async fn pkce_user_token(
    srv: &TestServer,
    app: &serde_json::Value,
    username: &str,
    verifier: &str,
    use_wrong_verifier: bool,
) -> (String, String) {
    let redirect_uri = "https://client.example/cb";
    let challenge = crypto::pkce_s256_challenge(verifier);
    let params = format!(
        "client_id={}&redirect_uri={}&response_type=code&scope=read&state=xyz&code_challenge={}&code_challenge_method=S256",
        enc(app["client_id"].as_str().unwrap()),
        enc(redirect_uri),
        enc(&challenge),
    );

    // Login submission: establishes a session cookie.
    let login_body = format!("username={}&password=password123&{params}", enc(username));
    let (status, headers, _bytes) = send(
        &srv.router,
        form_request("POST", "/oauth/authorize", &login_body, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = common::session_cookie(&headers).expect("session cookie");

    // Consent: approve and receive the redirect with the one-time code.
    let consent_body = format!("{params}&decision=authorize");
    let mut request = form_request("POST", "/oauth/authorize", &consent_body, None);
    request.headers_mut().insert(
        "cookie",
        cookie.parse().expect("valid cookie header"),
    );
    let (status, headers, body) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::FOUND, "consent: {}", String::from_utf8_lossy(&body));
    let location = headers
        .get("location")
        .expect("location")
        .to_str()
        .unwrap()
        .to_string();
    let code = query_param(&location, "code").expect("code param");

    // Exchange the code for a token.
    let presented = if use_wrong_verifier {
        "wrong-verifier-value-that-is-long-enough-000"
    } else {
        verifier
    };
    let exchange_body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}&client_id={}&client_secret={}",
        enc(&code),
        enc(redirect_uri),
        enc(presented),
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _headers, bytes) = send(
        &srv.router,
        form_request("POST", "/oauth/token", &exchange_body, None),
    )
    .await;
    let body = String::from_utf8_lossy(&bytes).to_string();
    (format!("{}|{body}", status.as_u16()), code)
}

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

#[tokio::test]
async fn app_registration_returns_credential_shape() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read write").await;

    assert_eq!(app["name"], "Test App");
    assert_eq!(app["scopes"], "read write");
    assert_eq!(app["redirect_uris"][0], "https://client.example/cb");
    assert_eq!(app["redirect_uri"], "https://client.example/cb");
    assert_eq!(app["client_secret_expires_at"], 0);
    assert!(app["client_id"].as_str().unwrap().len() >= 16);
    assert!(app["client_secret"].as_str().unwrap().len() == 64);
    assert_eq!(app["vapid_key"], "");
    assert!(app["id"].is_string());
}

#[tokio::test]
async fn app_registration_requires_name_and_redirect_uri() {
    let srv = TestServer::new(true);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/apps", "scopes=read", None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(json(&bytes)["error"].is_string());
}

#[tokio::test]
async fn client_credentials_issues_app_only_token() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read write").await;
    let token = client_credentials_token(&srv, &app, "read").await;

    // An app-only token may not act as a user.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "This method requires an authenticated user"
    );
}

#[tokio::test]
async fn client_credentials_rejects_scope_escalation() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("read write"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "invalid_scope");
}

#[tokio::test]
async fn invalid_client_secret_is_rejected() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret=not-the-secret",
        enc(app["client_id"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&bytes)["error"], "invalid_client");
}

#[tokio::test]
async fn unsupported_grant_type_is_rejected() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let body = format!(
        "grant_type=refresh_token&client_id={}&client_secret={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "unsupported_grant_type");
}

#[tokio::test]
async fn authorization_code_pkce_flow_issues_user_token() {
    let srv = TestServer::new(true);
    let account = common::mk_account(&srv.store, "alice");
    let app = register_app(&srv, "read write profile").await;

    // GET authorize renders the login page without a session.
    let (status, _h, _b) = send(
        &srv.router,
        get_request(
            &format!("/oauth/authorize?client_id={}", app["client_id"].as_str().unwrap()),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (result, _code) = pkce_user_token(&srv, &app, "alice", VERIFIER, false).await;
    assert!(result.starts_with("200|"), "exchange failed: {result}");
    let body: serde_json::Value =
        serde_json::from_str(result.split_once('|').unwrap().1).unwrap();
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["scope"], "read");
    let token = body["access_token"].as_str().unwrap();

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["id"], account.id.to_string());
}

#[tokio::test]
async fn wrong_pkce_verifier_is_invalid_grant() {
    let srv = TestServer::new(true);
    let _account = common::mk_account(&srv.store, "bob");
    let app = register_app(&srv, "read").await;
    let (result, _code) = pkce_user_token(&srv, &app, "bob", VERIFIER, true).await;
    assert!(result.starts_with("400|"), "expected failure: {result}");
    let body: serde_json::Value =
        serde_json::from_str(result.split_once('|').unwrap().1).unwrap();
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn authorization_code_is_single_use() {
    let srv = TestServer::new(true);
    let _account = common::mk_account(&srv.store, "carol");
    let app = register_app(&srv, "read").await;

    // Obtain a code by running the flow but intercept the exchange: reuse the
    // returned code by exchanging twice with the same verifier.
    let (result, code) = pkce_user_token(&srv, &app, "carol", VERIFIER, false).await;
    assert!(result.starts_with("200|"), "{result}");

    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}&client_id={}&client_secret={}",
        enc(&code),
        enc("https://client.example/cb"),
        enc(VERIFIER),
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "invalid_grant");
}

#[tokio::test]
async fn redirect_uri_mismatch_is_invalid_grant() {
    let srv = TestServer::new(true);
    let _account = common::mk_account(&srv.store, "dave");
    let app = register_app(&srv, "read").await;
    let (result, code) = pkce_user_token(&srv, &app, "dave", VERIFIER, false).await;
    assert!(result.starts_with("200|"), "{result}");

    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}&client_id={}&client_secret={}",
        enc(&code),
        enc("https://evil.example/cb"),
        enc(VERIFIER),
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "invalid_grant");
}

#[tokio::test]
async fn revoke_invalidates_token() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let token = client_credentials_token(&srv, &app, "read").await;

    let body = format!(
        "token={}&client_id={}&client_secret={}",
        enc(&token),
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/revoke", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"{}");

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/instance", Some(&token), None),
    )
    .await;
    // Instance is public, so a revoked token is simply anonymous: 200.
    assert_eq!(status, StatusCode::OK);
    let _ = bytes;

    // A revoked token presented to an authed endpoint is invalid.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/preferences", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&bytes)["error"], "The access token is invalid");
}

#[tokio::test]
async fn revoke_with_wrong_client_is_forbidden() {
    let srv = TestServer::new(true);
    let app_a = register_app(&srv, "read").await;
    let app_b = register_app(&srv, "read").await;
    let token = client_credentials_token(&srv, &app_a, "read").await;

    let body = format!(
        "token={}&client_id={}&client_secret={}",
        enc(&token),
        enc(app_b["client_id"].as_str().unwrap()),
        enc(app_b["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/revoke", &body, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "unauthorized_client");
}

#[tokio::test]
async fn revoke_empty_token_is_ok() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let body = format!(
        "token=&client_id={}&client_secret={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/revoke", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"{}");
}
