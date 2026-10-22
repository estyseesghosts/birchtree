use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Relationship writes (each returns a fresh RelationshipSerializer).
        .route(
            "/api/v1/accounts/:id/follow",
            post(endpoints::relationships::follow),
        )
        .route(
            "/api/v1/accounts/:id/unfollow",
            post(endpoints::relationships::unfollow),
        )
        .route(
            "/api/v1/accounts/:id/remove_from_followers",
            post(endpoints::relationships::remove_from_followers),
        )
        .route(
            "/api/v1/accounts/:id/block",
            post(endpoints::relationships::block),
        )
        .route(
            "/api/v1/accounts/:id/unblock",
            post(endpoints::relationships::unblock),
        )
        .route(
            "/api/v1/accounts/:id/mute",
            post(endpoints::relationships::mute),
        )
        .route(
            "/api/v1/accounts/:id/unmute",
            post(endpoints::relationships::unmute),
        )
        .route(
            "/api/v1/accounts/:id/note",
            post(endpoints::relationships::note),
        )
        // Account endorsements ("pins"): pin/endorse are aliases.
        .route(
            "/api/v1/accounts/:id/pin",
            post(endpoints::relationships::endorse),
        )
        .route(
            "/api/v1/accounts/:id/endorse",
            post(endpoints::relationships::endorse),
        )
        .route(
            "/api/v1/accounts/:id/unpin",
            post(endpoints::relationships::unendorse),
        )
        .route(
            "/api/v1/accounts/:id/unendorse",
            post(endpoints::relationships::unendorse),
        )
        // Account collections.
        .route(
            "/api/v1/accounts/:id/followers",
            get(endpoints::relationships::followers),
        )
        .route(
            "/api/v1/accounts/:id/following",
            get(endpoints::relationships::following),
        )
        .route(
            "/api/v1/accounts/:id/endorsements",
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
            "/api/v1/follow_requests/:id/authorize",
            post(endpoints::relationships::authorize_request),
        )
        .route(
            "/api/v1/follow_requests/:id/reject",
            post(endpoints::relationships::reject_request),
        )
        // Domain blocks (upstream shape: the domain is a parameter, not a path segment).
        .route(
            "/api/v1/domain_blocks",
            get(endpoints::relationships::domain_blocks_index)
                .post(endpoints::relationships::domain_blocks_create)
                .delete(endpoints::relationships::domain_blocks_delete),
        )
        // Domain-block preview (gaps G-C): `{domain, accounts_count}`.
        .route(
            "/api/v1/domain_blocks/preview",
            post(endpoints::collections::route_domain_blocks_preview),
        )
        // Preferences.
        .route("/api/v1/preferences", get(endpoints::preferences::index))
}
