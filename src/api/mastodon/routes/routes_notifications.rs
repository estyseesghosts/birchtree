use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Notifications (Phase 4). Static collection paths before `{id}`.
        .route(
            "/api/v1/notifications",
            get(endpoints::notifications::index),
        )
        .route(
            "/api/v1/notifications/clear",
            post(endpoints::notifications::clear),
        )
        .route(
            "/api/v1/notifications/unread_count",
            get(endpoints::notifications::unread_count),
        )
        .route(
            "/api/v1/notifications/requests",
            get(endpoints::notifications::requests_index),
        )
        .route(
            "/api/v1/notifications/requests/merged",
            get(endpoints::notifications::requests_merged),
        )
        // Notification-request pipeline (round 3, area C): upstream
        // `notifications/requests` member + bulk accept/dismiss and the
        // `notifications/policy` singleton (`config/routes/api.rb:178-194`).
        .route(
            "/api/v1/notifications/requests/:id",
            get(endpoints::notifications::requests_show),
        )
        .route(
            "/api/v1/notifications/requests/:id/accept",
            post(endpoints::notifications::requests_accept),
        )
        .route(
            "/api/v1/notifications/requests/:id/dismiss",
            post(endpoints::notifications::requests_dismiss),
        )
        .route(
            "/api/v1/notifications/requests/accept",
            post(endpoints::notifications::requests_accept_bulk),
        )
        .route(
            "/api/v1/notifications/requests/dismiss",
            post(endpoints::notifications::requests_dismiss_bulk),
        )
        .route(
            "/api/v1/notifications/policy",
            get(endpoints::notifications::policy_show).put(endpoints::notifications::policy_update),
        )
        .route(
            "/api/v1/notifications/:id",
            get(endpoints::notifications::show),
        )
        .route(
            "/api/v1/notifications/:id/dismiss",
            post(endpoints::notifications::dismiss),
        )
        // Grouped notifications (gaps G-C): static paths before `:group_key`.
        .route(
            "/api/v2/notifications",
            get(endpoints::collections::route_v2_index),
        )
        .route(
            "/api/v2/notifications/clear",
            post(endpoints::collections::route_v2_clear),
        )
        .route(
            "/api/v2/notifications/unread_count",
            get(endpoints::collections::route_v2_unread_count),
        )
        .route(
            "/api/v2/notifications/policy",
            get(endpoints::collections::route_v2_policy_show),
        )
        .route(
            "/api/v2/notifications/:group_key/dismiss",
            post(endpoints::collections::route_v2_dismiss),
        )
        .route(
            "/api/v2/notifications/:group_key/accounts",
            get(endpoints::collections::route_v2_group_accounts),
        )
        // Markers (Phase 4).
        .route(
            "/api/v1/markers",
            get(endpoints::markers::index).post(endpoints::markers::create),
        )
}
