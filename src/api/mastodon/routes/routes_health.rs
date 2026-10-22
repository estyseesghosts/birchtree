use crate::api::mastodon::ApiState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Runtime probes.
        .route("/health", get(health))
        .route(
            "/ready",
            get(crate::api::mastodon::router::readiness::ready),
        )
        .route(
            "/metrics",
            get(crate::api::mastodon::router::readiness::metrics),
        )
}

/// Liveness probe: the process is running.
async fn health() -> &'static str {
    "ok"
}
