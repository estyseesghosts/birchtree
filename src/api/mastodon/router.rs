//! Router assembly (declarative; endpoint files stay small).
//!
//! Routes are grouped by resource. Static paths are registered before dynamic
//! ones so `/api/v1/accounts/verify_credentials` never resolves as an account
//! id. Unimplemented endpoints are absent (they 404) rather than stubbed.

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, patch, post};
use tower_http::trace::TraceLayer;

use super::{ApiState, endpoints, rate_limits};
use crate::api::oauth::{authorization, token};

/// Build the application router.
pub fn build_router(state: ApiState) -> Router {
    let max_body = state.max_body_size;
    Router::new()
        // Runtime probes.
        .route("/health", get(health))
        .route("/ready", get(ready))
        // OAuth (browser authorize flow + token/revoke).
        .route(
            "/oauth/authorize",
            get(authorization::authorize_get).post(authorization::authorize_post),
        )
        .route("/oauth/token", post(token::token))
        .route("/oauth/revoke", post(token::revoke))
        // Instance.
        .route("/api/v1/instance", get(endpoints::instance::v1))
        .route("/api/v2/instance", get(endpoints::instance::v2))
        // Applications.
        .route("/api/v1/apps", post(endpoints::apps::create_app))
        // Accounts: static sub-paths before the `{id}` form.
        .route("/api/v1/accounts", post(endpoints::accounts::register))
        .route(
            "/api/v1/accounts/verify_credentials",
            get(endpoints::accounts::verify_credentials),
        )
        .route(
            "/api/v1/accounts/update_credentials",
            patch(endpoints::accounts::update_credentials),
        )
        .route(
            "/api/v1/accounts/relationships",
            get(endpoints::accounts::relationships),
        )
        .route("/api/v1/accounts/{id}", get(endpoints::accounts::show))
        // Preferences.
        .route("/api/v1/preferences", get(endpoints::preferences::index))
        .with_state(state)
        .layer(middleware::from_fn(rate_limits::middleware))
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
