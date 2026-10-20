//! Phase 4 search + custom emoji contract tests: `GET /api/v2/search` (v1
//! was removed upstream in Mastodon 3.0) and the public emoji index + image
//! serving.

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::Store;

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
        "client_name=Phase4Search&redirect_uris={}&scopes={}",
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

async fn user_token(srv: &TestServer, username: &str, scopes: &str) -> String {
    let app = register_app(srv, &format!("read write read:accounts write:accounts {scopes}")).await;
    register_account(srv, &app, username).await
}

// ---------------------------------------------------------------------------
// Search.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_requires_q_and_pages_with_mastodon_messages() {
    let srv = TestServer::new(true);

    // Missing q -> the ParameterMissing body.
    let (status, _h, bytes) = send(&srv.router, get_request("/api/v2/search", None, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"], "param is missing or the value is empty: q");

    // Anonymous with q: 200 and the exact four-array shape.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=anything", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let results = json(&bytes);
    assert_eq!(
        results.as_object().unwrap().keys().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        vec!["accounts", "collections", "hashtags", "statuses"]
    );
    assert!(results["collections"].as_array().unwrap().is_empty());

    // Anonymous pagination/resolve are refused with the pinned 401 bodies.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=x&offset=5", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(&bytes)["error"],
        "Search queries pagination is not supported without authentication"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=x&resolve=true", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(&bytes)["error"],
        "Search queries that resolve remote resources are not supported without authentication"
    );

    // Negative pagination -> 400 with the pinned message.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=x&offset=-1", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json(&bytes)["error"],
        "Pagination values for `offset` and `limit` must be positive"
    );
}

#[tokio::test]
async fn search_finds_accounts_hashtags_and_statuses() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice", "read:search read:statuses").await;

    // Account lookup: `alice` matches by handle.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=ali&type=accounts", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let results = json(&bytes);
    assert_eq!(results["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(results["accounts"][0]["username"], "alice");

    // A hashtag post surfaces in the hashtag search.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            "status=hello+%23Rustaceans+world",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=Rust&type=hashtags", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hashtags = json(&bytes)["hashtags"].clone();
    let hashtags = hashtags.as_array().unwrap();
    assert_eq!(hashtags.len(), 1, "prefix match on the normalized tag");
    assert_eq!(hashtags[0]["name"], "rustaceans");
    for key in ["id", "name", "url", "history", "following", "featuring"] {
        assert!(hashtags[0].get(key).is_some(), "missing {key}");
    }

    // Status search (authenticated): text match over recent statuses, with
    // the status DTO shape.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            "/api/v2/search?q=world&type=statuses",
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let statuses = json(&bytes)["statuses"].clone();
    assert_eq!(statuses.as_array().unwrap().len(), 1);
    assert!(statuses[0]["content"].as_str().unwrap().contains("world"));

    // Anonymous status search is empty (Mastodon's `status_searchable?`).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=world&type=statuses", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["statuses"].as_array().unwrap().len(), 0);

    // Unknown type: no branch runs -> all arrays empty (200).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/search?q=alice&type=gibberish", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let results = json(&bytes);
    assert!(results["accounts"].as_array().unwrap().is_empty());
    assert!(results["statuses"].as_array().unwrap().is_empty());
    assert!(results["hashtags"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_by_status_url_returns_the_stored_status() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice", "read:search read:statuses write:statuses").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=found+by+url", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let created = json(&bytes);
    let (id, uri) = (created["id"].as_str().unwrap(), created["uri"].as_str().unwrap());

    // No `type`: as Mastodon, a status URL query finds the status without `resolve`.
    let path = format!("/api/v2/search?q={}", enc(uri));
    let (status, _h, bytes) = send(&srv.router, get_request(&path, Some(&alice), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let statuses = json(&bytes)["statuses"].clone();
    assert_eq!(statuses.as_array().unwrap().len(), 1);
    assert_eq!(statuses[0]["id"], id);

    // Anonymous callers get nothing, as for any status search.
    let (status, _h, bytes) = send(&srv.router, get_request(&path, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes)["statuses"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Custom emoji.
// ---------------------------------------------------------------------------

/// A valid 1x1 PNG.
fn png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

#[tokio::test]
async fn emoji_index_is_public_and_serves_images() {
    let srv = TestServer::new(true);

    // Empty instance: a public, cacheable empty array.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/custom_emojis", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
    assert_eq!(
        headers.get(axum::http::header::CACHE_CONTROL).unwrap(),
        "max-age=300, public"
    );

    // Register an emoji through the use case (CLI path).
    let store: &dyn Store = &*srv.store;
    birchtree::application::emoji::add_emoji(
        store,
        &*srv.blobs,
        "party_blob",
        &png_bytes(),
        Some("Fun"),
        true,
    )
    .unwrap();
    // A hidden emoji stays out of the picker index.
    birchtree::application::emoji::add_emoji(
        store,
        &*srv.blobs,
        "secret_blob",
        &png_bytes(),
        None,
        false,
    )
    .unwrap();

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/custom_emojis", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let emojis = json(&bytes);
    let emojis = emojis.as_array().unwrap();
    assert_eq!(emojis.len(), 1, "only listed emoji are returned");
    let emoji = &emojis[0];
    // REST::CustomEmojiSerializer keys (category/featured are present for the
    // index endpoint).
    for key in ["shortcode", "url", "static_url", "visible_in_picker", "category", "featured"] {
        assert!(emoji.get(key).is_some(), "missing {key}");
    }
    assert_eq!(emoji["shortcode"], "party_blob");
    assert_eq!(emoji["category"], "Fun");
    assert_eq!(emoji["featured"], false);
    assert_eq!(emoji["visible_in_picker"], true);
    assert!(emoji["url"].as_str().unwrap().contains("/system/custom_emojis/images/"));

    // The image route serves the bytes with an image content type.
    let url = emoji["url"].as_str().unwrap();
    let path = url.trim_start_matches("https://example.social");
    let (status, headers, bytes) = send(&srv.router, get_request(path, None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(headers.get(axum::http::header::CONTENT_TYPE).unwrap(), "image/png");
    assert_eq!(bytes, png_bytes());

    // Unknown emoji path -> 404.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/system/custom_emojis/images/00/00/nope/original/x.png", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Invalid shortcodes are refused by the use case.
    let err = birchtree::application::emoji::add_emoji(store, &*srv.blobs, "x", &png_bytes(), None, true)
        .unwrap_err();
    assert!(matches!(err, birchtree::domain::Error::InvalidInput(_)));
}
