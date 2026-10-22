//! Collections routes (S2 slice group E).
//!
//! Source spans in `super::router` (`router.rs`): 244-251 (account-scoped,
//! kept here with their `collections::` handlers) + 831-856.
//! Skipped (left to sibling groups, path-based ownership):
//! - `router.rs:71-74` `/api/v1/instance/peers/search` -> routes_instance
//! - `router.rs:277-280` `/api/v1/domain_blocks/preview` -> routes_relationships
//! - `router.rs:344-347` `/api/v1/statuses/:id/in_collections` -> routes_statuses
//! - `router.rs:781-804` `/api/v2/notifications*` -> routes_notifications

use axum::Router;
use axum::routing::{get, post};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Account collections (gaps G-C): account-scoped reads with
        // `collections::` handlers (original position 244-251).
        .route(
            "/api/v1/accounts/:id/collections",
            get(endpoints::collections::route_account_collections),
        )
        .route(
            "/api/v1/accounts/:id/lists",
            get(endpoints::collections::route_account_lists),
        )
        // Collections (gaps G-C): top-level CRUD + items/revoke + the
        // minimal `v1_alpha` namespace.
        .route(
            "/api/v1/collections",
            get(endpoints::collections::route_collections_index)
                .post(endpoints::collections::route_collections_create),
        )
        .route(
            "/api/v1/collections/:id",
            get(endpoints::collections::route_collections_show)
                .put(endpoints::collections::route_collections_update)
                .patch(endpoints::collections::route_collections_update)
                .delete(endpoints::collections::route_collections_destroy),
        )
        .route(
            "/api/v1/collections/:id/items",
            get(endpoints::collections::route_items_index)
                .post(endpoints::collections::route_items_add)
                .delete(endpoints::collections::route_items_remove),
        )
        .route(
            "/api/v1/collections/:id/revoke",
            post(endpoints::collections::route_collections_revoke),
        )
        .route(
            "/api/v1_alpha/collections",
            get(endpoints::collections::route_alpha_collections_index),
        )
}
