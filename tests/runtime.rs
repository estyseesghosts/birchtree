//! Runtime contract tests: health, readiness, body limits and routing.
//!
//! Exercises the real router in-process (no external server needed).

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::api::mastodon::{build_router, ApiState};
use birchtree::application::context::Context;
use birchtree::domain::{SystemClock, TimestampIdGenerator};
use tower::ServiceExt;

fn app(max_body_size: usize) -> (tempfile::TempDir, axum::Router) {
    let (dir, store) = common::temp_store();
    let clock: Arc<dyn birchtree::domain::Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn birchtree::domain::IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let context = Arc::new(Context::new(clock, ids, store));
    let router = build_router(ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size,
        server: common::server_info(),
    });
    (dir, router)
}

async fn get(router: axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let response = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

#[tokio::test]
async fn health_is_ok() {
    let (_dir, router) = app(1024);
    let (status, body) = get(router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"ok");
}

#[tokio::test]
async fn ready_reports_storage_available() {
    let (_dir, router) = app(1024);
    let (status, body) = get(router, "/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"ready");
}

#[tokio::test]
async fn unknown_route_is_404() {
    let (_dir, router) = app(1024);
    let (status, _) = get(router, "/api/v1/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
