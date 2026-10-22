//! Admin routes (S2 slice group E).
//!
//! Source spans in `super::router` (`router.rs`): 311-329, 515-589, 598-645.
//! Takes ALL `/api/v1/admin/*` + `/api/v2/admin/*` direct routes plus the
//! `admin_infra` merge (which owns domain_allows/domain_blocks/
//! email_domain_blocks/ip_blocks/canonical_email_blocks/tags + measures /
//! dimensions / retention). Direct verbs here: get/post/put (delete/patch
//! live inside the `admin_infra` merge).

use axum::Router;
use axum::routing::{get, post, put};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;

pub fn router() -> Router<ApiState> {
    Router::new()
        // Announcements admin authoring (new API surface; upstream is HTML
        // only). Administration-only (Admin/Owner; Moderator lacks
        // `manage_announcements` — `config/roles.yml` at the pin).
        .route(
            "/api/v1/admin/announcements",
            get(endpoints::announcements::admin_index).post(endpoints::announcements::admin_create),
        )
        .route(
            "/api/v1/admin/announcements/:id",
            get(endpoints::announcements::admin_show)
                .put(endpoints::announcements::admin_update)
                .patch(endpoints::announcements::admin_update)
                .delete(endpoints::announcements::admin_destroy),
        )
        .route(
            "/api/v1/admin/announcements/:id/publish",
            post(endpoints::announcements::admin_publish),
        )
        .route(
            "/api/v1/admin/announcements/:id/unpublish",
            post(endpoints::announcements::admin_unpublish),
        )
        // Admin API (Phase 7.3): roles + scope families are enforced centrally
        // in each handler (role before scope).
        .route(
            "/api/v1/admin/accounts",
            get(endpoints::admin_accounts::index_v1),
        )
        .route(
            "/api/v2/admin/accounts",
            get(endpoints::admin_accounts::index_v2),
        )
        .route(
            "/api/v1/admin/accounts/:id",
            get(endpoints::admin_accounts::show).delete(endpoints::admin_accounts::destroy),
        )
        .route(
            "/api/v1/admin/accounts/:id/enable",
            post(endpoints::admin_accounts::enable),
        )
        .route(
            "/api/v1/admin/accounts/:id/unsensitive",
            post(endpoints::admin_accounts::unsensitive),
        )
        .route(
            "/api/v1/admin/accounts/:id/unsilence",
            post(endpoints::admin_accounts::unsilence),
        )
        .route(
            "/api/v1/admin/accounts/:id/unsuspend",
            post(endpoints::admin_accounts::unsuspend),
        )
        .route(
            "/api/v1/admin/accounts/:id/approve",
            post(endpoints::admin_accounts::approve),
        )
        .route(
            "/api/v1/admin/accounts/:id/reject",
            post(endpoints::admin_accounts::reject),
        )
        .route(
            "/api/v1/admin/accounts/:account_id/action",
            post(endpoints::admin_accounts::action),
        )
        .route(
            "/api/v1/admin/reports",
            get(endpoints::admin_reports::index),
        )
        .route(
            "/api/v1/admin/reports/:id",
            get(endpoints::admin_reports::show)
                .put(endpoints::admin_reports::update)
                .patch(endpoints::admin_reports::update),
        )
        .route(
            "/api/v1/admin/reports/:id/assign_to_self",
            post(endpoints::admin_reports::assign_to_self),
        )
        .route(
            "/api/v1/admin/reports/:id/unassign",
            post(endpoints::admin_reports::unassign),
        )
        .route(
            "/api/v1/admin/reports/:id/reopen",
            post(endpoints::admin_reports::reopen),
        )
        .route(
            "/api/v1/admin/reports/:id/resolve",
            post(endpoints::admin_reports::resolve),
        )
        .route(
            "/api/v1/admin/custom_emojis",
            get(endpoints::admin_emoji::index).post(endpoints::admin_emoji::create),
        )
        .route(
            "/api/v1/admin/custom_emojis/:shortcode",
            put(endpoints::admin_emoji::update).delete(endpoints::admin_emoji::destroy),
        )
        .merge(endpoints::admin_infra::routes())
        .route(
            "/api/v1/admin/trends/tags",
            get(endpoints::admin_trends::tags_index),
        )
        .route(
            "/api/v1/admin/trends/tags/:id/approve",
            post(endpoints::admin_trends::tags_approve),
        )
        .route(
            "/api/v1/admin/trends/tags/:id/reject",
            post(endpoints::admin_trends::tags_reject),
        )
        .route(
            "/api/v1/admin/trends/links",
            get(endpoints::admin_trends::links_index),
        )
        .route(
            "/api/v1/admin/trends/links/:id/approve",
            post(endpoints::admin_trends::links_approve),
        )
        .route(
            "/api/v1/admin/trends/links/:id/reject",
            post(endpoints::admin_trends::links_reject),
        )
        .route(
            "/api/v1/admin/trends/links/publishers",
            get(endpoints::admin_trends::publishers_index),
        )
        .route(
            "/api/v1/admin/trends/links/publishers/:id/approve",
            post(endpoints::admin_trends::publishers_approve),
        )
        .route(
            "/api/v1/admin/trends/links/publishers/:id/reject",
            post(endpoints::admin_trends::publishers_reject),
        )
        .route(
            "/api/v1/admin/trends/statuses",
            get(endpoints::admin_trends::statuses_index),
        )
        .route(
            "/api/v1/admin/trends/statuses/:id/approve",
            post(endpoints::admin_trends::statuses_approve),
        )
        .route(
            "/api/v1/admin/trends/statuses/:id/reject",
            post(endpoints::admin_trends::statuses_reject),
        )
}
