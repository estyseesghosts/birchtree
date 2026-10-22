//! Misc routes (S2 slice group E): everything not owned by a sibling group.
//!
//! Source spans in `super::router` (`router.rs`): 112, 285-292, 294-307,
//! 331-334, 400-448, 450-457, 459-512, 591-597, 647-666, 689-690, 858-887.
//! Verbs: get/post/put/patch/delete.
//!
//! Overlap candidates (TAKEN here, parent dedupes by line span):
//! - `router.rs:112` `/api/v1/apps` -> maybe routes_accounts/routes_oauth
//! - `router.rs:416-423` `/api/v1/statuses/:id/mute|unmute` -> routes_statuses
//! - `router.rs:450-453` `/api/v1/timelines/link` -> routes_timelines
//! - `router.rs:454-457` `/api/v1/apps/verify_credentials` -> routes_accounts
//! - `router.rs:459-509` relations_extra block (familiar_followers, blocks,
//!   mutes, favourites, bookmarks, featured_tags, followed_tags, tags/*)
//!   -> routes_relationships (path-adjacent)
//! - `router.rs:331-334` `/api/v1/donation_campaigns` -> unclaimed, taken here
//! Skipped (sibling-owned): reports (statuses), markers (notifications),
//! filters/lists (lists_filters), media/system (media), v1+v2 notifications
//! except none taken here, instance/*, accounts/*, statuses/*, timelines/*,
//! `/api/v1/preferences` + `/api/v1/follow_requests*` (relationships).

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Applications.
        .route("/api/v1/apps", post(endpoints::apps::create_app))
        // Suggestions (Phase 7): v1 member delete; v2 has no destroy route
        // upstream (`config/routes/api.rb:265`).
        .route("/api/v1/suggestions", get(endpoints::suggestions::v1))
        .route(
            "/api/v1/suggestions/:id",
            delete(endpoints::suggestions::delete_v1),
        )
        .route("/api/v2/suggestions", get(endpoints::suggestions::v2))
        // Profile directory (P1/S1).
        .route("/api/v1/directory", get(endpoints::directory::show))
        // Announcements (Phase 7).
        .route(
            "/api/v1/announcements",
            get(endpoints::announcements::index),
        )
        .route(
            "/api/v1/announcements/:id/dismiss",
            post(endpoints::announcements::dismiss),
        )
        .route(
            "/api/v1/announcements/:announcement_id/reactions/:id",
            put(endpoints::announcements::reaction_put)
                .patch(endpoints::announcements::reaction_put)
                .delete(endpoints::announcements::reaction_delete),
        )
        // Donation campaigns (Phase 7).
        .route(
            "/api/v1/donation_campaigns",
            get(endpoints::donation_campaigns::index),
        )
        // Conversations + conversation mutes (Phase 7).
        .route(
            "/api/v1/conversations",
            get(endpoints::conversations::index),
        )
        .route(
            "/api/v1/conversations/:id",
            delete(endpoints::conversations::destroy),
        )
        .route(
            "/api/v1/conversations/:id/read",
            post(endpoints::conversations::read),
        )
        .route(
            "/api/v1/conversations/:id/unread",
            post(endpoints::conversations::unread),
        )
        .route(
            "/api/v1/statuses/:id/mute",
            post(endpoints::conversations::mute_status),
        )
        .route(
            "/api/v1/statuses/:id/unmute",
            post(endpoints::conversations::unmute_status),
        )
        // Web Push (Phase 7).
        .route(
            "/api/v1/push/subscription",
            post(endpoints::push::create)
                .get(endpoints::push::show)
                .put(endpoints::push::update)
                .patch(endpoints::push::update)
                .delete(endpoints::push::destroy),
        )
        // Phase 7 first-party web surface (session + CSRF).
        .route(
            "/api/web/settings",
            put(endpoints::web::settings_update).patch(endpoints::web::settings_update),
        )
        .route("/api/web/embeds/:id", get(endpoints::web::embeds_show))
        .route(
            "/api/web/push_subscriptions",
            post(endpoints::web::push_create),
        )
        .route(
            "/api/web/push_subscriptions/:id",
            put(endpoints::web::push_update)
                .patch(endpoints::web::push_update)
                .delete(endpoints::web::push_destroy),
        )
        // Stray Phase 7 rows.
        .route(
            "/api/v1/apps/verify_credentials",
            get(endpoints::timelines_link::verify_app_credentials),
        )
        // Relation collection reads + tag surface (Phase 7).
        .route(
            "/api/v1/accounts/familiar_followers",
            get(endpoints::relations_extra::familiar_followers),
        )
        .route("/api/v1/blocks", get(endpoints::relations_extra::blocks))
        .route("/api/v1/mutes", get(endpoints::relations_extra::mutes))
        .route(
            "/api/v1/favourites",
            get(endpoints::relations_extra::favourites),
        )
        .route(
            "/api/v1/bookmarks",
            get(endpoints::relations_extra::bookmarks),
        )
        .route(
            "/api/v1/featured_tags",
            get(endpoints::relations_extra::featured_tags_index)
                .post(endpoints::relations_extra::featured_tags_create),
        )
        .route(
            "/api/v1/featured_tags/:id",
            delete(endpoints::relations_extra::featured_tags_destroy),
        )
        .route(
            "/api/v1/followed_tags",
            get(endpoints::relations_extra::followed_tags),
        )
        .route(
            "/api/v1/accounts/:id/featured_tags",
            get(endpoints::relations_extra::account_featured_tags),
        )
        .route(
            "/api/v1/tags/:id",
            get(endpoints::relations_extra::tag_show),
        )
        .route(
            "/api/v1/tags/:id/follow",
            post(endpoints::relations_extra::tag_follow),
        )
        .route(
            "/api/v1/tags/:id/unfollow",
            post(endpoints::relations_extra::tag_unfollow),
        )
        .route(
            "/api/v1/tags/:id/feature",
            post(endpoints::relations_extra::tag_feature),
        )
        .route(
            "/api/v1/tags/:id/unfeature",
            post(endpoints::relations_extra::tag_unfeature),
        )
        // keystodon first-party bundle (404 unless configured).
        .route("/app", get(endpoints::web::serve_bundle))
        .route("/app/*path", get(endpoints::web::serve_bundle))
        // Trends (Phase 7.3): public approved-only reads.
        .route("/api/v1/trends", get(endpoints::trends::trends_alias))
        .route("/api/v1/trends/tags", get(endpoints::trends::tags_index))
        .route("/api/v1/trends/links", get(endpoints::trends::links_index))
        .route(
            "/api/v1/trends/statuses",
            get(endpoints::trends::statuses_index),
        )
        // Annual reports (Phase 7.3): `:id` is the year.
        .route(
            "/api/v1/annual_reports",
            get(endpoints::annual_reports::index),
        )
        .route(
            "/api/v1/annual_reports/:id",
            get(endpoints::annual_reports::show),
        )
        .route(
            "/api/v1/annual_reports/:id/state",
            get(endpoints::annual_reports::show_state),
        )
        .route(
            "/api/v1/annual_reports/:id/generate",
            post(endpoints::annual_reports::generate),
        )
        .route(
            "/api/v1/annual_reports/:id/read",
            post(endpoints::annual_reports::mark_read),
        )
        // Polls.
        .route("/api/v1/polls/:id", get(endpoints::polls::show))
        .route("/api/v1/polls/:id/votes", post(endpoints::polls::vote))
        // Scheduled statuses (Phase 4).
        .route(
            "/api/v1/scheduled_statuses",
            get(endpoints::scheduled_statuses::index),
        )
        .route(
            "/api/v1/scheduled_statuses/:id",
            get(endpoints::scheduled_statuses::show),
        )
        .route(
            "/api/v1/scheduled_statuses/:id",
            put(endpoints::scheduled_statuses::update),
        )
        .route(
            "/api/v1/scheduled_statuses/:id",
            patch(endpoints::scheduled_statuses::update),
        )
        .route(
            "/api/v1/scheduled_statuses/:id",
            delete(endpoints::scheduled_statuses::destroy),
        )
        // Streaming (Phase 5): SSE and WebSocket share one handler.
        .route("/api/v1/streaming", get(crate::streaming::http::endpoint))
        .route(
            "/api/v1/streaming/*any",
            get(crate::streaming::http::endpoint),
        )
        // Search (Phase 4: v2 only; v1 was removed upstream in Mastodon 3.0).
        .route("/api/v2/search", get(endpoints::search::index))
        // Custom emoji (Phase 4: public index).
        .route("/api/v1/custom_emojis", get(endpoints::emoji::index))
}
