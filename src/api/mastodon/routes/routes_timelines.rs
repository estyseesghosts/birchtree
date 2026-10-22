use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Stray Phase 7 rows.
        .route(
            "/api/v1/timelines/link",
            get(endpoints::timelines_link::link),
        )
        // Timelines (Phase 4).
        .route("/api/v1/timelines/home", get(endpoints::timelines::home))
        .route(
            "/api/v1/timelines/public",
            get(endpoints::timelines::public),
        )
        .route("/api/v1/timelines/tag/:tag", get(endpoints::timelines::tag))
        .route(
            "/api/v1/timelines/list/:id",
            get(endpoints::timelines::list),
        )
}
