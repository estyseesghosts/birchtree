//! Router assembly (declarative; endpoint files stay small).
//!
//! Routes are grouped by resource. Static paths are registered before dynamic
//! ones so `/api/v1/accounts/verify_credentials` never resolves as an account
//! id. Unimplemented endpoints are absent (they 404) rather than stubbed.

use axum::Router;
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
        .with_state(state)
        // Malformed JSON is refused before any handler runs (Rails parses
        // params before the action); this sits inside the rate limiter, which
        // runs first, as Rack::Attack does upstream.
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
