//! Phase 4 scheduled-status contract tests: `scheduled_at` on create, the
//! exact validation bodies, CRUD, and the durable publish job (destroy row
//! first, replay params through the normal creation path).

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::JobQueue;

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
        "client_name=Phase4Sched&redirect_uris={}&scopes={}",
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

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(
        srv,
        "read write read:statuses write:statuses read:accounts write:accounts",
    )
    .await;
    register_account(srv, &app, username).await
}

/// Run every pending durable job once (claiming at a far-future clock so
/// scheduled rows due later are also claimable when the test wants them).
fn run_jobs(srv: &TestServer, at_ms: u64) {
    use birchtree::domain::JobKind;
    let mail = birchtree::mail::LoggingMailSender::default();
    for job in srv.store.claim_due(at_ms, 64).unwrap() {
        match job.kind {
            // Fail-closed mail (prod-2 item 4): without SMTP the job refuses;
            // complete it so scheduled jobs still run in the harness.
            JobKind::SendConfirmationEmail | JobKind::SendPasswordResetEmail => {
                assert!(birchtree::jobs::handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &birchtree::application::ports::NoStreaming).is_err());
                srv.store.complete(job.id).unwrap();
            }
            _ => {
                birchtree::jobs::handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &birchtree::application::ports::NoStreaming).unwrap();
                srv.store.complete(job.id).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn scheduling_returns_the_scheduled_shape_and_publishes_later() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "alice").await;

    // One hour from now (well past the 5-minute floor).
    let in_one_hour = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let in_one_hour = {
        let dt = chrono::DateTime::parse_from_rfc3339(&in_one_hour).unwrap();
        dt.timestamp() + 3_600
    };
    let scheduled_at = format!(
        "{}Z",
        chrono::DateTime::from_timestamp(in_one_hour, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .trim_end_matches('Z')
    );

    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!(
                "status={}&scheduled_at={}&visibility=unlisted&spoiler_text=cw",
                enc("later post"),
                enc(&scheduled_at)
            ),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let scheduled = json(&bytes);
    assert_eq!(
        scheduled.as_object().unwrap().keys().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        vec!["id", "media_attachments", "params", "scheduled_at"]
    );
    assert_eq!(scheduled["params"]["text"], "later post");
    assert_eq!(scheduled["params"]["spoiler_text"], "cw");
    assert_eq!(scheduled["params"]["visibility"], "unlisted");
    assert!(scheduled["params"]["scheduled_at"].is_null());
    assert!(scheduled["params"]["idempotency"].is_null());
    assert!(scheduled["media_attachments"].as_array().unwrap().is_empty());
    let id = scheduled["id"].as_str().unwrap().to_string();

    // Index + show.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/scheduled_statuses", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json(&bytes);
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["id"], id.as_str());
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/scheduled_statuses/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["id"], id.as_str());

    // No status exists yet.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/statuses", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);

    // Reschedule (only scheduled_at is editable).
    let in_two_hours_ts = in_one_hour + 3_600;
    let in_two_hours = format!(
        "{}Z",
        chrono::DateTime::from_timestamp(in_two_hours_ts, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .trim_end_matches('Z')
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/scheduled_statuses/{id}"),
            &format!("scheduled_at={}", enc(&in_two_hours)),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["params"]["text"], "later post");

    // The publish job claims at the due time; running it posts the status
    // (destroying the scheduled row first).
    run_jobs(&srv, in_one_hour as u64 * 1_000);
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/scheduled_statuses", Some(&token), None),
    )
    .await;
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1, "not due yet");

    run_jobs(&srv, (in_two_hours_ts + 60) as u64 * 1_000);
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/scheduled_statuses", Some(&token), None),
    )
    .await;
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0, "published");
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    let alice_id = json(&bytes)["id"].as_str().unwrap().to_string();
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/accounts/{alice_id}/statuses"),
            Some(&token),
            None,
        ),
    )
    .await;
    let statuses = json(&bytes);
    assert_eq!(statuses.as_array().unwrap().len(), 1);
    assert_eq!(statuses[0]["content"], "<p>later post</p>");
    assert_eq!(statuses[0]["visibility"], "unlisted");
    assert_eq!(statuses[0]["spoiler_text"], "cw");

    // Replaying the job (crash between handler and completion) is a no-op:
    // the row is gone and no duplicate status appears.
    run_jobs(&srv, (in_two_hours_ts + 60) as u64 * 1_000);
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/accounts/{alice_id}/statuses"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(json(&bytes).as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn scheduling_validation_matches_mastodon() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "alice").await;

    // Past/now -> silently posts immediately (regular status shape).
    let past = "2020-01-01T00:00:00Z";
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!("status=now&scheduled_at={}", enc(past)),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let posted = json(&bytes);
    assert!(posted.get("uri").is_some(), "regular status shape");

    // Unparseable -> 422 Record invalid.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            "status=bad&scheduled_at=not-a-date",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "Record invalid");

    // Inside the 5-minute floor (but still future) -> the exact validation
    // body; a past time posts immediately instead.
    let soon_ts = chrono::Utc::now().timestamp() + 60;
    let soon = format!(
        "{}Z",
        chrono::DateTime::from_timestamp(soon_ts, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .trim_end_matches('Z')
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!("status=soon&scheduled_at={}", enc(&soon)),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Scheduled at date must be in the future"
    );

    // A foreign scheduled id is a 404 with the pinned body.
    let other = user_token(&srv, "bob").await;
    let in_an_hour = chrono::Utc::now().timestamp() + 3_600;
    let scheduled_at = format!(
        "{}Z",
        chrono::DateTime::from_timestamp(in_an_hour, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .trim_end_matches('Z')
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!("status=bob&scheduled_at={}", enc(&scheduled_at)),
            Some(&other),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let id = json(&bytes)["id"].as_str().unwrap().to_string();
    for (method, path) in [
        ("GET", format!("/api/v1/scheduled_statuses/{id}")),
        ("PUT", format!("/api/v1/scheduled_statuses/{id}")),
        ("DELETE", format!("/api/v1/scheduled_statuses/{id}")),
    ] {
        let (status, _h, bytes) = send(
            &srv.router,
            form_request(method, &path, "scheduled_at=2030-01-01T00:00:00Z", Some(&token)),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(json(&bytes)["error"], "Record not found");
    }

    // App-only tokens are refused (require_user).
    let app = register_app(&srv, "write").await;
    let app_tok = app_token(&srv, &app, "write").await;
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=x", Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "This method requires an authenticated user");
}

#[tokio::test]
async fn delete_scheduled_status_returns_empty_object() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "alice").await;
    let in_an_hour = chrono::Utc::now().timestamp() + 3_600;
    let scheduled_at = format!(
        "{}Z",
        chrono::DateTime::from_timestamp(in_an_hour, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .trim_end_matches('Z')
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!("status=deleteme&scheduled_at={}", enc(&scheduled_at)),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = json(&bytes)["id"].as_str().unwrap().to_string();

    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "DELETE",
            &format!("/api/v1/scheduled_statuses/{id}"),
            "",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));

    // The publish job for a deleted row is a no-op.
    run_jobs(&srv, (in_an_hour as u64 + 60) * 1_000);
    let (_status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/scheduled_statuses", Some(&token), None),
    )
    .await;
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);
}
