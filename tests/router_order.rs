//! S2 route-order snapshot: the router split must not add, remove, or
//! reorder routes in a way that changes matching.
//!
//! - `route_fragment_count` pins the per-file `.route(` totals (246 total).
//!   Adding a route means updating the snapshot here in the same commit.
//! - `verify_credentials_resolves_before_id` proves static paths still win
//!   over `/:id` (`verify_credentials` without auth is 401 from the static
//!   credentials handler, while an unknown id is 404).

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use birchtree::api::mastodon::router::{build_router, readiness::RuntimeProbes};
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::context::Context;
use birchtree::config::Config;
use birchtree::domain::{Clock, IdGenerator, SystemClock, TimestampIdGenerator};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

/// Count `.route(` call sites in a route-fragment source file.
fn count_routes(source: &str) -> usize {
    source.matches(".route(").count()
}

#[test]
fn route_fragment_count() {
    let files = [
        ("routes_health", include_str!("../src/api/mastodon/routes/routes_health.rs"), 3),
        ("routes_oauth", include_str!("../src/api/mastodon/routes/routes_oauth.rs"), 7),
        ("routes_instance", include_str!("../src/api/mastodon/routes/routes_instance.rs"), 13),
        ("routes_accounts", include_str!("../src/api/mastodon/routes/routes_accounts.rs"), 15),
        ("routes_relationships", include_str!("../src/api/mastodon/routes/routes_relationships.rs"), 22),
        ("routes_collections", include_str!("../src/api/mastodon/routes/routes_collections.rs"), 7),
        ("routes_statuses", include_str!("../src/api/mastodon/routes/routes_statuses.rs"), 26),
        ("routes_misc", include_str!("../src/api/mastodon/routes/routes_misc.rs"), 57),
        ("routes_admin", include_str!("../src/api/mastodon/routes/routes_admin.rs"), 34),
        ("routes_timelines", include_str!("../src/api/mastodon/routes/routes_timelines.rs"), 5),
        ("routes_media", include_str!("../src/api/mastodon/routes/routes_media.rs"), 9),
        ("routes_notifications", include_str!("../src/api/mastodon/routes/routes_notifications.rs"), 20),
        ("routes_lists_filters", include_str!("../src/api/mastodon/routes/routes_lists_filters.rs"), 28),
    ];
    let mut total = 0;
    for (name, source, expected) in files {
        let found = count_routes(source);
        assert_eq!(found, expected, "route count drift in {name}");
        total += found;
    }
    // 246 `.route(` calls in the pre-split `router.rs`; the admin_infra
    // `.merge()` in routes_admin covers its own sub-tree on top.
    assert_eq!(total, 246, "total route count drift");
}

fn open_with(dir: &std::path::Path, ids: Arc<dyn IdGenerator>) -> RedbStore {
    RedbStore::open(dir.join("birchtree.redb"), Arc::new(SystemClock), ids).expect("open store")
}

fn api_state(dir: &std::path::Path) -> ApiState {
    let store = Arc::new(open_with(dir, Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)))));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blob store"),
    );
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        transcoding_enabled: false,
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let streams = Hub::new(store.clone(), clock.clone(), server.clone(), Limits::default());
    let context = Arc::new(Context::new(clock, ids, store.clone(), blobs, Arc::new(streams.clone())));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 1024 * 1024,
        server,
        streams,
        federation,
        rate_limit_multiplier: 1,
        mail_env: Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: store as Arc<dyn birchtree::application::accounts::InviteStore>,
    }
}

async fn get_status(router: &axum::Router, path: &str) -> StatusCode {
    let request = Request::builder().uri(path).body(Body::empty()).expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    response.status()
}

#[tokio::test]
async fn health_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let router = build_router(api_state(dir.path()));
    assert_eq!(get_status(&router, "/health").await, StatusCode::OK);
}

#[tokio::test]
async fn verify_credentials_resolves_before_id() {
    let _ = RuntimeProbes::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let router = build_router(api_state(dir.path()));
    // Static credentials route: no token -> 401 (not a 404 from `:id`).
    assert_eq!(
        get_status(&router, "/api/v1/accounts/verify_credentials").await,
        StatusCode::UNAUTHORIZED
    );
    // Unknown dynamic id -> 404.
    assert_eq!(
        get_status(&router, "/api/v1/accounts/123456789").await,
        StatusCode::NOT_FOUND
    );
}
