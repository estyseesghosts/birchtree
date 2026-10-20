//! Router assembly (declarative; endpoint files stay small).
//!
//! Routes are grouped by resource. Static paths are registered before dynamic
//! ones so `/api/v1/accounts/verify_credentials` never resolves as an account
//! id. Unimplemented endpoints are absent (they 404) rather than stubbed.

use axum::Router;
use axum::Extension;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::middleware;
use axum::routing::{delete, get, patch, post, put};
use tower_http::trace::TraceLayer;

use super::{ApiState, endpoints, rate_limits};
use crate::api::json_body;
use crate::api::oauth::{authorization, token};

/// Build the application router.
pub fn build_router(state: ApiState) -> Router {
    let max_body = state.max_body_size;
    let rate_multiplier = state.rate_limit_multiplier;
    let ip_block_state = state.clone();
    let invites = state.invites.clone();
    let mail_env = state.mail_env.clone();
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
        // Email confirmation redemption (D12): browser link from the mailed
        // token; success vs invalid/used shapes, no JS.
        .route(
            "/auth/confirmation",
            get(crate::api::oauth::confirmation::confirmation_get),
        )
        // Instance.
        .route("/api/v1/instance", get(endpoints::instance::v1))
        .route("/api/v2/instance", get(endpoints::instance::v2))
        // Applications.
        .route("/api/v1/apps", post(endpoints::apps::create_app))
        // Accounts: static sub-paths before the `{id}` form.
        .route("/api/v1/accounts", get(endpoints::accounts::index).post(endpoints::accounts::register))
        .route("/api/v1/accounts/lookup", get(endpoints::accounts::lookup))
        .route("/api/v1/accounts/search", get(endpoints::accounts::search))
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
        .route("/api/v1/accounts/:id", get(endpoints::accounts::show))
        // Invites + email. Without a mail provider the mail endpoints
        // answer an explicit 503 (never fake success); invites read the
        // configured store (durable redb in production).
        .route("/invite/:invite_code", get(endpoints::accounts::invite_show))
        .route(
            "/api/v1/emails/confirmations",
            post(endpoints::accounts::email_confirmation_create),
        )
        .route(
            "/api/v1/emails/check_confirmation",
            get(endpoints::accounts::email_confirmation_check),
        )
        .route(
            "/api/v1/emails/password_reset",
            post(endpoints::accounts::password_reset_create),
        )
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
        // Preferences.
        .route("/api/v1/preferences", get(endpoints::preferences::index))

        // Suggestions (Phase 7).
        .route("/api/v1/suggestions", get(endpoints::suggestions::v1).delete(endpoints::suggestions::delete_v1))
        .route("/api/v2/suggestions", get(endpoints::suggestions::v2).delete(endpoints::suggestions::delete_v2))
        // Announcements (Phase 7).
        .route("/api/v1/announcements", get(endpoints::announcements::index))
        .route("/api/v1/announcements/:id/dismiss", post(endpoints::announcements::dismiss))
        .route("/api/v1/announcements/:announcement_id/reactions/:id", put(endpoints::announcements::reaction_put).patch(endpoints::announcements::reaction_put).delete(endpoints::announcements::reaction_delete))
        // Donation campaigns (Phase 7).
        .route("/api/v1/donation_campaigns", get(endpoints::donation_campaigns::index))
        // Statuses: multi-fetch + CRUD (static `{id}` sub-routes after the
        // collection routes).
        .route("/api/v1/statuses", post(endpoints::statuses::create))
        .route("/api/v1/statuses", get(endpoints::statuses::index))
        .route("/api/v1/statuses/:id", get(endpoints::statuses::show))
        .route("/api/v1/statuses/:id", put(endpoints::statuses::update))
        .route("/api/v1/statuses/:id", patch(endpoints::statuses::update))
        .route(
            "/api/v1/statuses/:id",
            delete(endpoints::statuses::destroy),
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
        // Conversations + conversation mutes (Phase 7).
        .route("/api/v1/conversations", get(endpoints::conversations::index))
        .route("/api/v1/conversations/:id", delete(endpoints::conversations::destroy))
        .route("/api/v1/conversations/:id/read", post(endpoints::conversations::read))
        .route("/api/v1/conversations/:id/unread", post(endpoints::conversations::unread))
        .route("/api/v1/statuses/:id/mute", post(endpoints::conversations::mute_status))
        .route("/api/v1/statuses/:id/unmute", post(endpoints::conversations::unmute_status))
        // Web Push (Phase 7).
        .route("/api/v1/push/subscription", post(endpoints::push::create).get(endpoints::push::show).put(endpoints::push::update).patch(endpoints::push::update).delete(endpoints::push::destroy))
        // Phase 7 first-party web surface (session + CSRF).
        .route("/api/web/settings", put(endpoints::web::settings_update).patch(endpoints::web::settings_update))
        .route("/api/web/embeds/:id", get(endpoints::web::embeds_show))
        .route("/api/web/push_subscriptions", post(endpoints::web::push_create))
        .route("/api/web/push_subscriptions/:id", put(endpoints::web::push_update).patch(endpoints::web::push_update).delete(endpoints::web::push_destroy))
        // Stray Phase 7 rows.
        .route("/api/v1/timelines/link", get(endpoints::timelines_link::link))
        .route("/api/v1/apps/verify_credentials", get(endpoints::timelines_link::verify_app_credentials))
        // Relation collection reads + tag surface (Phase 7).
        .route("/api/v1/accounts/familiar_followers", get(endpoints::relations_extra::familiar_followers))
        .route("/api/v1/blocks", get(endpoints::relations_extra::blocks))
        .route("/api/v1/mutes", get(endpoints::relations_extra::mutes))
        .route("/api/v1/favourites", get(endpoints::relations_extra::favourites))
        .route("/api/v1/bookmarks", get(endpoints::relations_extra::bookmarks))
        .route("/api/v1/featured_tags", get(endpoints::relations_extra::featured_tags_index).post(endpoints::relations_extra::featured_tags_create))
        .route("/api/v1/featured_tags/:id", delete(endpoints::relations_extra::featured_tags_destroy))
        .route("/api/v1/followed_tags", get(endpoints::relations_extra::followed_tags))
        .route("/api/v1/accounts/:id/featured_tags", get(endpoints::relations_extra::account_featured_tags))
        .route("/api/v1/tags/:id", get(endpoints::relations_extra::tag_show))
        .route("/api/v1/tags/:id/follow", post(endpoints::relations_extra::tag_follow))
        .route("/api/v1/tags/:id/unfollow", post(endpoints::relations_extra::tag_unfollow))
        .route("/api/v1/tags/:id/feature", post(endpoints::relations_extra::tag_feature))
        .route("/api/v1/tags/:id/unfeature", post(endpoints::relations_extra::tag_unfeature))
        // keystodon first-party bundle (404 unless configured).
        .route("/app", get(endpoints::web::serve_bundle))
        .route("/app/*path", get(endpoints::web::serve_bundle))
        // Admin API (Phase 7.3): roles + scope families are enforced centrally
        // in each handler (role before scope).
        .route("/api/v1/admin/accounts", get(endpoints::admin_accounts::index_v1))
        .route("/api/v2/admin/accounts", get(endpoints::admin_accounts::index_v2))
        .route("/api/v1/admin/accounts/:id", get(endpoints::admin_accounts::show).delete(endpoints::admin_accounts::destroy))
        .route("/api/v1/admin/accounts/:id/enable", post(endpoints::admin_accounts::enable))
        .route("/api/v1/admin/accounts/:id/unsensitive", post(endpoints::admin_accounts::unsensitive))
        .route("/api/v1/admin/accounts/:id/unsilence", post(endpoints::admin_accounts::unsilence))
        .route("/api/v1/admin/accounts/:id/unsuspend", post(endpoints::admin_accounts::unsuspend))
        .route("/api/v1/admin/accounts/:id/approve", post(endpoints::admin_accounts::approve))
        .route("/api/v1/admin/accounts/:id/reject", post(endpoints::admin_accounts::reject))
        .route("/api/v1/admin/accounts/:account_id/action", post(endpoints::admin_accounts::action))
        .route("/api/v1/admin/reports", get(endpoints::admin_reports::index))
        .route("/api/v1/admin/reports/:id", get(endpoints::admin_reports::show).put(endpoints::admin_reports::update).patch(endpoints::admin_reports::update))
        .route("/api/v1/admin/reports/:id/assign_to_self", post(endpoints::admin_reports::assign_to_self))
        .route("/api/v1/admin/reports/:id/unassign", post(endpoints::admin_reports::unassign))
        .route("/api/v1/admin/reports/:id/reopen", post(endpoints::admin_reports::reopen))
        .route("/api/v1/admin/reports/:id/resolve", post(endpoints::admin_reports::resolve))
        .merge(endpoints::admin_infra::routes())
        // Trends (Phase 7.3): public approved-only reads.
        .route("/api/v1/trends", get(endpoints::trends::trends_alias))
        .route("/api/v1/trends/tags", get(endpoints::trends::tags_index))
        .route("/api/v1/trends/links", get(endpoints::trends::links_index))
        .route("/api/v1/trends/statuses", get(endpoints::trends::statuses_index))
        .route("/api/v1/admin/trends/tags", get(endpoints::admin_trends::tags_index))
        .route("/api/v1/admin/trends/tags/:id/approve", post(endpoints::admin_trends::tags_approve))
        .route("/api/v1/admin/trends/tags/:id/reject", post(endpoints::admin_trends::tags_reject))
        .route("/api/v1/admin/trends/links", get(endpoints::admin_trends::links_index))
        .route("/api/v1/admin/trends/links/:id/approve", post(endpoints::admin_trends::links_approve))
        .route("/api/v1/admin/trends/links/:id/reject", post(endpoints::admin_trends::links_reject))
        .route("/api/v1/admin/trends/links/publishers", get(endpoints::admin_trends::publishers_index))
        .route("/api/v1/admin/trends/links/publishers/:id/approve", post(endpoints::admin_trends::publishers_approve))
        .route("/api/v1/admin/trends/links/publishers/:id/reject", post(endpoints::admin_trends::publishers_reject))
        .route("/api/v1/admin/trends/statuses", get(endpoints::admin_trends::statuses_index))
        .route("/api/v1/admin/trends/statuses/:id/approve", post(endpoints::admin_trends::statuses_approve))
        .route("/api/v1/admin/trends/statuses/:id/reject", post(endpoints::admin_trends::statuses_reject))
        // Annual reports (Phase 7.3): `:id` is the year.
        .route("/api/v1/annual_reports", get(endpoints::annual_reports::index))
        .route("/api/v1/annual_reports/:id", get(endpoints::annual_reports::show))
        .route("/api/v1/annual_reports/:id/state", get(endpoints::annual_reports::show_state))
        .route("/api/v1/annual_reports/:id/generate", post(endpoints::annual_reports::generate))
        .route("/api/v1/annual_reports/:id/read", post(endpoints::annual_reports::mark_read))
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
        // Polls.
        .route("/api/v1/polls/:id", get(endpoints::polls::show))
        .route(
            "/api/v1/polls/:id/votes",
            post(endpoints::polls::vote),
        )
        // Timelines (Phase 4).
        .route(
            "/api/v1/timelines/home",
            get(endpoints::timelines::home),
        )
        .route(
            "/api/v1/timelines/public",
            get(endpoints::timelines::public),
        )
        .route(
            "/api/v1/timelines/tag/:tag",
            get(endpoints::timelines::tag),
        )
        .route(
            "/api/v1/timelines/list/:id",
            get(endpoints::timelines::list),
        )
        // Media (Phase 4): v1 + v2 upload, per-id CRUD.
        .route("/api/v1/media", post(endpoints::media::create))
        .route("/api/v2/media", post(endpoints::media::create_v2))
        .route("/api/v1/media/:id", get(endpoints::media::show))
        .route("/api/v1/media/:id", put(endpoints::media::update))
        .route("/api/v1/media/:id", patch(endpoints::media::update))
        .route("/api/v1/media/:id", delete(endpoints::media::destroy))
        // Local file serving (Mastodon storage path shapes).
        .route(
            "/system/media_attachments/files/*path",
            get(endpoints::media::serve_file),
        )
        .route(
            "/system/custom_emojis/images/*path",
            get(endpoints::emoji::serve_image),
        )
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
            get(endpoints::notifications::policy_show)
                .put(endpoints::notifications::policy_update),
        )
        .route(
            "/api/v1/notifications/:id",
            get(endpoints::notifications::show),
        )
        .route(
            "/api/v1/notifications/:id/dismiss",
            post(endpoints::notifications::dismiss),
        )
        // Markers (Phase 4).
        .route(
            "/api/v1/markers",
            get(endpoints::markers::index).post(endpoints::markers::create),
        )
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
        .route("/api/v1/streaming/*any", get(crate::streaming::http::endpoint))
        // Search (Phase 4: v2 only; v1 was removed upstream in Mastodon 3.0).
        .route("/api/v2/search", get(endpoints::search::index))
        // Custom emoji (Phase 4: public index).
        .route(
            "/api/v1/custom_emojis",
            get(endpoints::emoji::index),
        )
        // Filters (Phase 4): v1 synthesized from v2, then v2 CRUD + rules.
        .route("/api/v1/filters", get(endpoints::filters::v1_index))
        .route("/api/v1/filters", post(endpoints::filters::v1_create))
        .route("/api/v1/filters/:id", get(endpoints::filters::v1_show))
        .route("/api/v1/filters/:id", put(endpoints::filters::v1_update))
        .route(
            "/api/v1/filters/:id",
            delete(endpoints::filters::v1_destroy),
        )
        .route("/api/v2/filters", get(endpoints::filters::v2_index))
        .route("/api/v2/filters", post(endpoints::filters::v2_create))
        .route("/api/v2/filters/keywords/:id", get(endpoints::filters::keywords_show))
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
        .route("/api/v2/filters/:id", patch(endpoints::filters::v2_update))
        .route("/api/v2/filters/:id", delete(endpoints::filters::v2_destroy))
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
        // Federation (Phase 6): WebFinger, NodeInfo, the actor document and the
        // inboxes. The crate's middleware sits on these routes only.
        .merge(crate::activitypub::http::routes::<ApiState>(state.federation.clone()))
        .layer(Extension(invites))
        .layer(Extension(mail_env))
        .with_state(state)
        // Malformed JSON is refused before any handler runs (Rails parses
        // params before the action); this sits inside the rate limiter, which
        // runs first, as Rack::Attack does upstream.
        .layer(middleware::from_fn_with_state(ip_block_state, endpoints::admin_infra::ip_block_middleware))
        .layer(middleware::from_fn(json_body::guard))
        .layer(middleware::from_fn_with_state(rate_multiplier, rate_limits::middleware))
        .layer(middleware::from_fn(vary_authorization))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(TraceLayer::new_for_http())
}

/// `Vary: Authorization` on API responses (Mastodon's `vary_by
/// 'Authorization'` on every API controller), so shared caches never mix
/// personalized and anonymous payloads.
async fn vary_authorization(
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if is_api {
        response.headers_mut().insert(
            axum::http::header::VARY,
            axum::http::header::HeaderValue::from_static("Authorization"),
        );
    }
    response
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
