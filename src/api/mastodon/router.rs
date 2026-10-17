//! Router assembly (declarative; endpoint files stay small).
//!
//! Routes are grouped by resource. Static paths are registered before dynamic
//! ones so `/api/v1/accounts/verify_credentials` never resolves as an account
//! id. Unimplemented endpoints are absent (they 404) rather than stubbed.

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{delete, get, patch, post, put};
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
        // Relationship writes (each returns a fresh RelationshipSerializer).
        .route(
            "/api/v1/accounts/{id}/follow",
            post(endpoints::relationships::follow),
        )
        .route(
            "/api/v1/accounts/{id}/unfollow",
            post(endpoints::relationships::unfollow),
        )
        .route(
            "/api/v1/accounts/{id}/remove_from_followers",
            post(endpoints::relationships::remove_from_followers),
        )
        .route(
            "/api/v1/accounts/{id}/block",
            post(endpoints::relationships::block),
        )
        .route(
            "/api/v1/accounts/{id}/unblock",
            post(endpoints::relationships::unblock),
        )
        .route(
            "/api/v1/accounts/{id}/mute",
            post(endpoints::relationships::mute),
        )
        .route(
            "/api/v1/accounts/{id}/unmute",
            post(endpoints::relationships::unmute),
        )
        .route(
            "/api/v1/accounts/{id}/note",
            post(endpoints::relationships::note),
        )
        // Account endorsements ("pins"): pin/endorse are aliases.
        .route(
            "/api/v1/accounts/{id}/pin",
            post(endpoints::relationships::endorse),
        )
        .route(
            "/api/v1/accounts/{id}/endorse",
            post(endpoints::relationships::endorse),
        )
        .route(
            "/api/v1/accounts/{id}/unpin",
            post(endpoints::relationships::unendorse),
        )
        .route(
            "/api/v1/accounts/{id}/unendorse",
            post(endpoints::relationships::unendorse),
        )
        // Account collections.
        .route(
            "/api/v1/accounts/{id}/followers",
            get(endpoints::relationships::followers),
        )
        .route(
            "/api/v1/accounts/{id}/following",
            get(endpoints::relationships::following),
        )
        .route(
            "/api/v1/accounts/{id}/endorsements",
            get(endpoints::relationships::account_endorsements),
        )
        .route(
            "/api/v1/endorsements",
            get(endpoints::relationships::endorsements_index),
        )
        // Follow requests (locked accounts).
        .route(
            "/api/v1/follow_requests",
            get(endpoints::relationships::follow_requests),
        )
        .route(
            "/api/v1/follow_requests/{id}/authorize",
            post(endpoints::relationships::authorize_request),
        )
        .route(
            "/api/v1/follow_requests/{id}/reject",
            post(endpoints::relationships::reject_request),
        )
        // Domain blocks.
        .route(
            "/api/v1/domain_blocks",
            get(endpoints::relationships::domain_blocks_index)
                .put(endpoints::relationships::domain_blocks_create),
        )
        .route(
            "/api/v1/domain_blocks/{domain}",
            delete(endpoints::relationships::domain_blocks_delete),
        )
        // Preferences.
        .route("/api/v1/preferences", get(endpoints::preferences::index))
        // Statuses: multi-fetch + CRUD (static `{id}` sub-routes after the
        // collection routes).
        .route("/api/v1/statuses", post(endpoints::statuses::create))
        .route("/api/v1/statuses", get(endpoints::statuses::index))
        .route("/api/v1/statuses/{id}", get(endpoints::statuses::show))
        .route("/api/v1/statuses/{id}", put(endpoints::statuses::update))
        .route("/api/v1/statuses/{id}", patch(endpoints::statuses::update))
        .route(
            "/api/v1/statuses/{id}",
            delete(endpoints::statuses::destroy),
        )
        .route(
            "/api/v1/statuses/{id}/source",
            get(endpoints::statuses::source),
        )
        .route(
            "/api/v1/statuses/{id}/context",
            get(endpoints::statuses::context),
        )
        .route(
            "/api/v1/statuses/{id}/history",
            get(endpoints::statuses::history),
        )
        .route(
            "/api/v1/statuses/{id}/favourite",
            post(endpoints::statuses::favourite),
        )
        .route(
            "/api/v1/statuses/{id}/unfavourite",
            post(endpoints::statuses::unfavourite),
        )
        .route(
            "/api/v1/statuses/{id}/bookmark",
            post(endpoints::statuses::bookmark),
        )
        .route(
            "/api/v1/statuses/{id}/unbookmark",
            post(endpoints::statuses::unbookmark),
        )
        .route(
            "/api/v1/statuses/{id}/reblog",
            post(endpoints::statuses::reblog),
        )
        .route(
            "/api/v1/statuses/{id}/unreblog",
            post(endpoints::statuses::unreblog),
        )
        .route("/api/v1/statuses/{id}/pin", post(endpoints::statuses::pin))
        .route(
            "/api/v1/statuses/{id}/unpin",
            post(endpoints::statuses::unpin),
        )
        .route(
            "/api/v1/statuses/{id}/favourited_by",
            get(endpoints::statuses::favourited_by),
        )
        .route(
            "/api/v1/statuses/{id}/reblogged_by",
            get(endpoints::statuses::reblogged_by),
        )
        .route(
            "/api/v1/statuses/{id}/quotes",
            get(endpoints::statuses::quotes_index),
        )
        .route(
            "/api/v1/statuses/{id}/quotes/{quoting_id}/revoke",
            post(endpoints::statuses::quotes_revoke),
        )
        .route(
            "/api/v1/statuses/{id}/interaction_policy",
            put(endpoints::statuses::interaction_policy),
        )
        .route(
            "/api/v1/statuses/{id}/interaction_policy",
            patch(endpoints::statuses::interaction_policy),
        )
        // Account statuses (pinned list included: `?pinned=true`).
        .route(
            "/api/v1/accounts/{id}/statuses",
            get(endpoints::statuses::account_statuses),
        )
        // Polls.
        .route("/api/v1/polls/{id}", get(endpoints::polls::show))
        .route(
            "/api/v1/polls/{id}/votes",
            post(endpoints::polls::vote),
        )
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
