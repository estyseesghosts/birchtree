//! Instance routes (S2 slice group B fragment).

use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Instance.
        .route("/api/v1/instance", get(endpoints::instance::v1))
        .route("/api/v2/instance", get(endpoints::instance::v2))
        // Instance sub-resources (fix round; all v1-only upstream).
        .route(
            "/api/v1/instance/peers",
            get(endpoints::instance::peers_index),
        )
        // Peer search (gaps G-C): substring-filtered known remote domains.
        .route(
            "/api/v1/instance/peers/search",
            get(endpoints::collections::route_peers_search),
        )
        .route(
            "/api/v1/instance/rules",
            get(endpoints::instance::rules_index),
        )
        .route(
            "/api/v1/instance/domain_blocks",
            get(endpoints::instance::domain_blocks_index),
        )
        .route(
            "/api/v1/instance/terms_of_service",
            get(endpoints::instance::terms_of_service_index),
        )
        .route(
            "/api/v1/instance/terms_of_service/:date",
            get(endpoints::instance::terms_of_service_show),
        )
        .route(
            "/api/v1/instance/privacy_policy",
            get(endpoints::instance::privacy_policy_show),
        )
        .route(
            "/api/v1/instance/extended_description",
            get(endpoints::instance::extended_description_show),
        )
        .route(
            "/api/v1/instance/translation_languages",
            get(endpoints::instance::translation_languages_show),
        )
        .route(
            "/api/v1/instance/languages",
            get(endpoints::instance::languages_show),
        )
        .route(
            "/api/v1/instance/activity",
            get(endpoints::instance::activity_show),
        )
}
