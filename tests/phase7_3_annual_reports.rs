//! Phase 7.3 annual-report contract tests (Subagent D).
//!
//! Index/show/state reads (`read|read:accounts`), generation as a durable
//! job and mark-read (`write|write:accounts`), plus the auth matrix. Report
//! data is computed from real local records; an empty year reports honest
//! zeros. Router wiring lives with the main session; these tests mount the
//! same handlers on a test router sharing one `ApiState`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get as route_get, post as route_post};

use birchtree::api::mastodon::endpoints::annual_reports;
use birchtree::api::mastodon::ApiState;
use birchtree::application::context::Context;
use birchtree::application::ports::JobQueue;
use birchtree::domain::{Clock, IdGenerator, SystemClock, TimestampIdGenerator};
use common::{TestServer, federation_config, form_request, get_request, json, send, server_info, TEST_PUBLIC_URL};

struct Harness {
    srv: TestServer,
    router: axum::Router,
}

impl Harness {
    fn new() -> Self {
        let srv = TestServer::new(true);
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> =
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
        let streams = srv.streams.clone();
        let context = Arc::new(Context::new(
            clock,
            ids,
            srv.store.clone(),
            srv.blobs.clone(),
            Arc::new(streams.clone()),
        ));
        let state = ApiState {
            context: context.clone(),
            trusted_proxies: Arc::new(Vec::new()),
            max_body_size: 16 * 1024 * 1024,
            server: server_info(),
            streams: streams.clone(),
            federation: federation_config(context, TEST_PUBLIC_URL),
            rate_limit_multiplier: 1,
        };
        let router = axum::Router::new()
            .route("/api/v1/annual_reports", route_get(annual_reports::index))
            .route("/api/v1/annual_reports/:id", route_get(annual_reports::show))
            .route("/api/v1/annual_reports/:id/state", route_get(annual_reports::show_state))
            .route(
                "/api/v1/annual_reports/:id/generate",
                route_post(annual_reports::generate),
            )
            .route(
                "/api/v1/annual_reports/:id/read",
                route_post(annual_reports::mark_read),
            )
            .with_state(state);
        Self { srv, router }
    }
}

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

const SCOPES: &str = "read write read:accounts write:accounts read:statuses write:statuses";

async fn user_token(h: &Harness, username: &str) -> String {
    let body = format!(
        "client_name=Annual&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(SCOPES),
    );
    let (status, _h, bytes) =
        send(&h.srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("write:accounts"),
    );
    let (status, _h, bytes) =
        send(&h.srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_tok = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &h.srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn post_status(h: &Harness, token: &str, text: &str) {
    let body = format!("status={}", enc(text));
    let (status, _h, bytes) = send(
        &h.srv.router,
        form_request("POST", "/api/v1/statuses", &body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "post: {}", String::from_utf8_lossy(&bytes));
}

async fn get(router: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let (status, _h, bytes) = send(router, get_request(uri, token, None)).await;
    (status, json(&bytes))
}

async fn post_empty(router: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let (status, _h, bytes) = send(router, form_request("POST", uri, "", token)).await;
    // Upstream answers `head 202` (empty body) while generation runs and
    // `200 {}` once the report exists; accept both shapes here.
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        json(&bytes)
    };
    (status, body)
}

/// Run every pending durable job once (the generation job included).
fn run_jobs(h: &Harness) {
    use birchtree::domain::Clock;
    let now = birchtree::domain::SystemClock.now_ms();
    let mail = birchtree::mail::LoggingMailSender::default();
    for job in h.srv.store.claim_due(now, 64).unwrap() {
        birchtree::jobs::handlers::handle(
            &job,
            &*h.srv.store,
            &mail,
            &*h.srv.blobs,
            &birchtree::application::ports::NoStreaming,
        )
        .unwrap();
        h.srv.store.complete(job.id).unwrap();
    }
}

fn this_year() -> i32 {
    chrono::Utc::now().format("%Y").to_string().parse().unwrap()
}

#[tokio::test]
async fn empty_index_then_generate_run_show_read() {
    let h = Harness::new();
    let year = this_year();
    let token = user_token(&h, "alice").await;

    // No reports: the index is empty but contract-shaped.
    let (status, body) = get(&h.router, "/api/v1/annual_reports", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["annual_reports"], serde_json::json!([]));
    assert_eq!(body["accounts"], serde_json::json!([]));
    assert_eq!(body["statuses"], serde_json::json!([]));

    // Unknown year: 404 on show, `ineligible` state (no statuses), generate
    // still enqueues (2000 predates every status id on this server).
    let (status, body) = get(&h.router, "/api/v1/annual_reports/2000", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], serde_json::json!("Record not found"));
    let (status, body) = get(&h.router, "/api/v1/annual_reports/2000/state", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": "ineligible"}));

    // Post real statuses, then generate the current year.
    post_status(&h, &token, "first #year").await;
    post_status(&h, &token, "second #year").await;

    let (status, body) = get(&h.router, &format!("/api/v1/annual_reports/{year}/state"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": "eligible"}));

    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/generate"), Some(&token)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // A second generate while the job is pending is also 202 (no duplicate).
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/generate"), Some(&token)).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, body) = get(&h.router, &format!("/api/v1/annual_reports/{year}/state"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": "generating"}));

    run_jobs(&h);

    let (status, body) = get(&h.router, &format!("/api/v1/annual_reports/{year}/state"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"state": "available"}));

    // The generated report carries real stats.
    let (status, body) = get(&h.router, &format!("/api/v1/annual_reports/{year}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["annual_reports"].as_array().unwrap().len(), 1);
    let report = &body["annual_reports"][0];
    assert_eq!(report["year"], serde_json::json!(year));
    assert_eq!(report["schema_version"], serde_json::json!(1));
    assert_eq!(report["share_url"], serde_json::Value::Null);
    assert_eq!(report["data"]["statuses_count"], serde_json::json!(2));
    assert_eq!(report["data"]["top_statuses"].as_array().unwrap().len(), 2);
    // The referenced statuses are embedded as full status payloads.
    assert_eq!(body["statuses"].as_array().unwrap().len(), 2);
    assert_eq!(body["accounts"].as_array().unwrap().len(), 1);

    // The pending index lists it; a repeat generate is a 200 no-op.
    let (status, body) = get(&h.router, "/api/v1/annual_reports", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["annual_reports"].as_array().unwrap().len(), 1);
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/generate"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);

    // Mark read: the pending index empties, but show still serves it.
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/read"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = get(&h.router, "/api/v1/annual_reports", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["annual_reports"], serde_json::json!([]));
    let (status, _) = get(&h.router, &format!("/api/v1/annual_reports/{year}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    // Reading a missing report is a 404.
    let (status, _) = post_empty(&h.router, "/api/v1/annual_reports/2000/read", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn empty_year_reports_honest_zeros() {
    let h = Harness::new();
    let token = user_token(&h, "carol").await;
    let (status, _) = post_empty(&h.router, "/api/v1/annual_reports/2000/generate", Some(&token)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    run_jobs(&h);
    let (status, body) = get(&h.router, "/api/v1/annual_reports/2000", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let report = &body["annual_reports"][0];
    assert_eq!(report["data"]["statuses_count"], serde_json::json!(0));
    assert_eq!(report["data"]["top_statuses"], serde_json::json!([]));
    assert_eq!(report["data"]["top_hashtags"], serde_json::json!([]));
}

#[tokio::test]
async fn annual_reports_auth_matrix() {
    let h = Harness::new();
    let year = this_year();
    let _token = user_token(&h, "dave").await;

    // Anonymous: 401 on every route.
    for (method, uri) in [
        ("GET", "/api/v1/annual_reports"),
        ("GET", &format!("/api/v1/annual_reports/{year}")),
        ("GET", &format!("/api/v1/annual_reports/{year}/state")),
    ] {
        let _ = method;
        let (status, _) = get(&h.router, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/generate"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/read"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Write-only token: reads are 403 (missing read scope).
    let body = format!(
        "client_name=Annual2&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("write write:accounts"),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app2 = json(&bytes);
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app2["client_id"].as_str().unwrap()),
        enc(app2["client_secret"].as_str().unwrap()),
        enc("write:accounts"),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app_tok = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let body = "username=erin&email=erin%40example.com&password=password123&agreement=true&locale=en";
    let (status, _h, bytes) = send(
        &h.srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "erin register: {}", String::from_utf8_lossy(&bytes));
    let write_only = json(&bytes)["access_token"].as_str().unwrap().to_string();

    let (status, body) = get(&h.router, "/api/v1/annual_reports", Some(&write_only)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], serde_json::json!("This action is outside the authorized scopes"));
    // ...but generation is allowed with the write scope.
    let (status, _) = post_empty(&h.router, &format!("/api/v1/annual_reports/{year}/generate"), Some(&write_only)).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // App-only token (no user): 422 on every route. This needs an app
    // carrying a read scope (the registration app above is write-only).
    let body = format!(
        "client_name=Annual3&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read read:accounts write write:accounts"),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app3 = json(&bytes);
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app3["client_id"].as_str().unwrap()),
        enc(app3["client_secret"].as_str().unwrap()),
        enc("read read:accounts"),
    );
    let (status, _h, bytes) = send(&h.srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app_only = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let (status, body) = get(&h.router, "/api/v1/annual_reports", Some(&app_only)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], serde_json::json!("This method requires an authenticated user"));
}
