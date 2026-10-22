//! Phase 4 notification contract tests: the read surface (index with
//! filters + `Link` cursors, show, dismiss, clear, `unread_count`) driven
//! through the real router, plus markers (optimistic versions) and the
//! notification-requests surface.

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::JobQueue;
use birchtree::jobs::handlers;

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
        "client_name=Phase4Notif&redirect_uris={}&scopes={}",
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
    let app = register_app(srv, &format!("read:accounts write:accounts follow {scopes}")).await;
    register_account(srv, &app, username).await
}

/// Run every pending durable job once (notifications are post-commit work).
fn drain_jobs(srv: &TestServer) {
    use birchtree::domain::JobKind;
    let mail = birchtree::mail::LoggingMailSender::default();
    // A far-future clock claims every due job (the lease math needs headroom).
    for job in srv.store.claim_due(4_000_000_000_000, 64).unwrap() {
        match job.kind {
            // Fail-closed mail (prod-2 item 4): without an SMTP provider the
            // confirmation job must refuse (never `Ok`). Complete it here so
            // unrelated notification jobs still run in the harness.
            JobKind::SendConfirmationEmail | JobKind::SendPasswordResetEmail => {
                assert!(
                    handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &birchtree::application::ports::NoStreaming).is_err(),
                    "mail without a provider must refuse"
                );
                srv.store.complete(job.id).unwrap();
            }
            _ => {
                handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &birchtree::application::ports::NoStreaming).unwrap();
                srv.store.complete(job.id).unwrap();
            }
        }
    }
}

/// Post a status as `token`; returns the status id.
async fn post_status(srv: &TestServer, token: &str, text: &str) -> String {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", &format!("status={}", enc(text)), Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn index_shows_interactions_with_exact_shape_and_filters() {
    let srv = TestServer::new(true);
    let bob = user_token(&srv, "bob", "read:notifications write:statuses write:favourites").await;
    let alice = user_token(&srv, "alice", "read:notifications write:statuses write:favourites").await;

    let status_id = post_status(&srv, &bob, "hello notifications").await;

    // Alice favourites + reblogs + follows bob.
    for path in [
        format!("/api/v1/statuses/{status_id}/favourite"),
        format!("/api/v1/statuses/{status_id}/reblog"),
    ] {
        let (status, _h, bytes) =
            send(&srv.router, form_request("POST", &path, "", Some(&alice))).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    }
    let bob_account_id = json(
        &send(
            &srv.router,
            get_request("/api/v1/accounts/verify_credentials", Some(&bob), None),
        )
        .await
        .2,
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{bob_account_id}/follow"),
            "",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    drain_jobs(&srv);

    // Index: newest first, exact key set.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let items = json(&bytes);
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 3, "favourite + reblog + follow");
    let mut ids: Vec<u64> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap().parse().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.reverse();
    ids = sorted;
    assert_eq!(ids.len(), 3);
    for item in items {
        assert_eq!(
            item.as_object().unwrap().keys().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
            vec!["account", "created_at", "group_key", "id", "status", "type"]
        );
        assert!(item["group_key"]
            .as_str()
            .unwrap()
            .starts_with("ungrouped-"));
    }
    let types: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert!(types.contains(&"favourite"));
    assert!(types.contains(&"reblog"));
    assert!(types.contains(&"follow"));
    // Status-bearing types carry a status; `follow` carries null.
    for item in items {
        if item["type"] == "follow" {
            assert!(item["status"].is_null());
        } else {
            assert!(!item["status"].is_null());
            assert_eq!(item["status"]["id"].as_str().is_some(), true);
        }
    }
    // Link cursors.
    let link = headers
        .get(axum::http::header::LINK)
        .expect("Link header on a non-empty page")
        .to_str()
        .unwrap()
        .to_string();
    let first = items[0]["id"].as_str().unwrap();
    let last = items[items.len() - 1]["id"].as_str().unwrap();
    assert!(link.contains(&format!("min_id={first}")), "{link}");
    assert!(link.contains(&format!("max_id={last}")), "{link}");

    // exclude_types[] drops the excluded kinds.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            "/api/v1/notifications?exclude_types[]=follow&exclude_types[]=reblog",
            Some(&bob),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json(&bytes);
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["type"], "favourite");

    // types[] narrows to the intersection (unknown values match nothing).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications?types[]=favourite", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications?types[]=gibberish", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);

    // limit=0 -> empty page (Mastodon `limit_param`).
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications?limit=0", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
    assert!(headers.get(axum::http::header::LINK).is_none());
}

#[tokio::test]
async fn show_dismiss_clear_and_unread_count() {
    let srv = TestServer::new(true);
    let bob = user_token(
        &srv,
        "bob",
        "read:notifications write:notifications write:statuses write:favourites",
    )
    .await;
    let alice = user_token(&srv, "alice", "read:notifications write:statuses write:favourites").await;
    let status_id = post_status(&srv, &bob, "interact with me").await;
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            "",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drain_jobs(&srv);

    // unread_count starts at 1.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/unread_count", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({ "count": 1 }));

    // Show the single notification.
    let (_status, _h, index_bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", Some(&bob), None),
    )
    .await;
    let id = json(&index_bytes)[0]["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/notifications/{id}"), Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["id"], id.as_str());
    assert_eq!(json(&bytes)["type"], "favourite");

    // Someone else's notification id is a 404 with the pinned body.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/notifications/{id}"), Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["error"], "Record not found");

    // Dismiss removes exactly one; a second dismiss is a 404.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/notifications/{id}/dismiss"),
            "",
            Some(&bob),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/notifications/{id}/dismiss"),
            "",
            Some(&bob),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Re-favourite creates a fresh notification; clear wipes everything.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            "",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "refavourite: {}", String::from_utf8_lossy(&bytes));
    drain_jobs(&srv);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/notifications/clear", "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", Some(&bob), None),
    )
    .await;
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/unread_count", Some(&bob), None),
    )
    .await;
    assert_eq!(json(&bytes), serde_json::json!({ "count": 0 }));
}

#[tokio::test]
async fn markers_track_last_read_and_bump_versions() {
    let srv = TestServer::new(true);
    // Markers use read/write:statuses scopes (Mastodon's markers controller).
    let bob = user_token(
        &srv,
        "bob",
        "read:notifications read:statuses write:statuses write:favourites",
    )
    .await;
    let alice = user_token(
        &srv,
        "alice",
        "read:notifications read:statuses write:statuses write:favourites",
    )
    .await;
    let status_id = post_status(&srv, &bob, "marker fuel").await;
    let (_status, _h, _bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/statuses/{status_id}/favourite"),
            "",
            Some(&alice),
        ),
    )
    .await;
    drain_jobs(&srv);

    // No markers yet.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/markers?timeline[]=home&timeline[]=notifications", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));

    // Set the notifications marker to the newest notification id.
    let (_status, _h, index_bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", Some(&bob), None),
    )
    .await;
    let newest = json(&index_bytes)[0]["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/markers",
            &format!("notifications[last_read_id]={newest}"),
            Some(&bob),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let marker = json(&bytes);
    assert_eq!(marker["notifications"]["last_read_id"], newest.as_str());
    assert_eq!(marker["notifications"]["version"], 1);
    assert!(marker["notifications"]["updated_at"].is_string());

    // GET returns only the requested timelines that exist.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/markers?timeline[]=home&timeline[]=notifications", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let marker = json(&bytes);
    assert!(marker.get("home").is_none());
    assert_eq!(marker["notifications"]["last_read_id"], newest.as_str());

    // unread_count now counts only rows newer than the marker.
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/unread_count", Some(&bob), None),
    )
    .await;
    assert_eq!(json(&bytes), serde_json::json!({ "count": 0 }));

    // A second update bumps the optimistic version.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/markers",
            &format!("notifications[last_read_id]={newest}&home[last_read_id]={newest}"),
            Some(&bob),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let marker = json(&bytes);
    assert_eq!(marker["notifications"]["version"], 2);
    assert_eq!(marker["home"]["last_read_id"], newest.as_str());
}

#[tokio::test]
async fn requests_surface_is_honest_and_scopes_are_enforced() {
    let srv = TestServer::new(true);
    let bob = user_token(&srv, "bob", "read:notifications").await;

    // Requests are always empty (no notification-policy filter exists yet).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/requests", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications/requests/merged", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({ "merged": true }));

    // Wrong scope on a write.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/notifications/clear", "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "This action is outside the authorized scopes");

    // App-only token on a user endpoint.
    let app = register_app(&srv, "read:notifications").await;
    let app_tok = app_token(&srv, &app, "read:notifications").await;
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", Some(&app_tok), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "This method requires an authenticated user");

    // No token.
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/v1/notifications", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
