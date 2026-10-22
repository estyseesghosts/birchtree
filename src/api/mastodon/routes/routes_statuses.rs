use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{delete, get, patch, post, put};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Statuses: multi-fetch + CRUD (static `{id}` sub-routes after the
        // collection routes).
        .route("/api/v1/statuses", post(endpoints::statuses::create))
        .route("/api/v1/statuses", get(endpoints::statuses::index))
        .route("/api/v1/statuses/:id", get(endpoints::statuses::show))
        .route("/api/v1/statuses/:id", put(endpoints::statuses::update))
        .route("/api/v1/statuses/:id", patch(endpoints::statuses::update))
        .route("/api/v1/statuses/:id", delete(endpoints::statuses::destroy))
        // The viewer's collections containing the status (gaps G-C).
        .route(
            "/api/v1/statuses/:id/in_collections",
            get(endpoints::collections::route_in_collections),
        )
        .route(
            "/api/v1/statuses/:id/source",
            get(endpoints::statuses::source),
        )
        .route(
            "/api/v1/statuses/:id/context",
            get(endpoints::statuses::context),
        )
        .route(
            "/api/v1/statuses/:id/history",
            get(endpoints::statuses::history),
        )
        .route(
            "/api/v1/statuses/:id/favourite",
            post(endpoints::statuses::favourite),
        )
        .route(
            "/api/v1/statuses/:id/unfavourite",
            post(endpoints::statuses::unfavourite),
        )
        .route(
            "/api/v1/statuses/:id/bookmark",
            post(endpoints::statuses::bookmark),
        )
        .route(
            "/api/v1/statuses/:id/unbookmark",
            post(endpoints::statuses::unbookmark),
        )
        .route(
            "/api/v1/statuses/:id/reblog",
            post(endpoints::statuses::reblog),
        )
        .route(
            "/api/v1/statuses/:id/unreblog",
            post(endpoints::statuses::unreblog),
        )
        .route("/api/v1/statuses/:id/pin", post(endpoints::statuses::pin))
        .route(
            "/api/v1/statuses/:id/unpin",
            post(endpoints::statuses::unpin),
        )
        .route(
            "/api/v1/statuses/:id/favourited_by",
            get(endpoints::statuses::favourited_by),
        )
        .route(
            "/api/v1/statuses/:id/reblogged_by",
            get(endpoints::statuses::reblogged_by),
        )
        // Reports (Phase 6): filed here; forwarded to a remote server as a Flag when asked.
        .route("/api/v1/reports", post(endpoints::reports::create))
        .route(
            "/api/v1/statuses/:id/quotes",
            get(endpoints::statuses::quotes_index),
        )
        .route(
            "/api/v1/statuses/:id/quotes/:quoting_id/revoke",
            post(endpoints::statuses::quotes_revoke),
        )
        .route(
            "/api/v1/statuses/:id/interaction_policy",
            put(endpoints::statuses::interaction_policy),
        )
        .route(
            "/api/v1/statuses/:id/interaction_policy",
            patch(endpoints::statuses::interaction_policy),
        )
        // Account statuses (pinned list included: `?pinned=true`).
        .route(
            "/api/v1/accounts/:id/statuses",
            get(endpoints::statuses::account_statuses),
        )
}
