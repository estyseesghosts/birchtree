//! Router assembly (declarative; endpoint files stay small).
//!
//! Phase 1 exposes runtime probes only. Endpoint routes are added per phase and
//! each unimplemented route stays explicitly out of profile until then.

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

use super::ApiState;

/// Build the application router.
pub fn build_router(state: ApiState) -> Router {
    let max_body = state.max_body_size;
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_body))
        .layer(TraceLayer::new_for_http())
}

/// Liveness probe: the process is running.
async fn health() -> &'static str {
    "ok"
}

/// Readiness probe: storage is open and can serve a read.
async fn ready(State(state): State<ApiState>) -> (StatusCode, &'static str) {
    if state.context.store.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}
