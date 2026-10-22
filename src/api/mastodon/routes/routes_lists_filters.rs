use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{delete, get, patch, post, put};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Lists (Phase 4). Collection paths before `{id}`.
        .route("/api/v1/lists", get(endpoints::lists::index))
        .route("/api/v1/lists", post(endpoints::lists::create))
        .route("/api/v1/lists/:id", get(endpoints::lists::show))
        .route("/api/v1/lists/:id", put(endpoints::lists::update))
        .route("/api/v1/lists/:id", patch(endpoints::lists::update))
        .route("/api/v1/lists/:id", delete(endpoints::lists::destroy))
        .route(
            "/api/v1/lists/:id/accounts",
            get(endpoints::lists::accounts_index),
        )
        .route(
            "/api/v1/lists/:id/accounts",
            post(endpoints::lists::accounts_create),
        )
        .route(
            "/api/v1/lists/:id/accounts",
            delete(endpoints::lists::accounts_destroy),
        )
        // Filters (Phase 4): v1 synthesized from v2, then v2 CRUD + rules.
        .route("/api/v1/filters", get(endpoints::filters::v1_index))
        .route("/api/v1/filters", post(endpoints::filters::v1_create))
        .route("/api/v1/filters/:id", get(endpoints::filters::v1_show))
        .route(
            "/api/v1/filters/:id",
            put(endpoints::filters::v1_update).patch(endpoints::filters::v1_update),
        )
        .route(
            "/api/v1/filters/:id",
            delete(endpoints::filters::v1_destroy),
        )
        .route("/api/v2/filters", get(endpoints::filters::v2_index))
        .route("/api/v2/filters", post(endpoints::filters::v2_create))
        .route(
            "/api/v2/filters/keywords/:id",
            get(endpoints::filters::keywords_show),
        )
        .route(
            "/api/v2/filters/keywords/:id",
            patch(endpoints::filters::keywords_update),
        )
        .route(
            "/api/v2/filters/keywords/:id",
            delete(endpoints::filters::keywords_destroy),
        )
        .route(
            "/api/v2/filters/statuses/:id",
            get(endpoints::filters::statuses_show),
        )
        .route(
            "/api/v2/filters/statuses/:id",
            delete(endpoints::filters::statuses_destroy),
        )
        .route("/api/v2/filters/:id", get(endpoints::filters::v2_show))
        .route(
            "/api/v2/filters/:id",
            put(endpoints::filters::v2_update).patch(endpoints::filters::v2_update),
        )
        .route(
            "/api/v2/filters/:id",
            delete(endpoints::filters::v2_destroy),
        )
        .route(
            "/api/v2/filters/:filter_id/keywords",
            get(endpoints::filters::keywords_index),
        )
        .route(
            "/api/v2/filters/:filter_id/keywords",
            post(endpoints::filters::keywords_create),
        )
        .route(
            "/api/v2/filters/:filter_id/statuses",
            get(endpoints::filters::statuses_index),
        )
        .route(
            "/api/v2/filters/:filter_id/statuses",
            post(endpoints::filters::statuses_create),
        )
}
