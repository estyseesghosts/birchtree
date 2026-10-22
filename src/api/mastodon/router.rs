//! Router assembly (S2 decomposition): one fragment per resource.
//!
//! Routes live in [`super::routes`] (one file per resource, merged here in
//! registration order). Static paths are registered before dynamic ones
//! within each fragment so `/api/v1/accounts/verify_credentials` never
//! resolves as an account id. Unimplemented endpoints are absent (they 404)
//! rather than stubbed.

use axum::Extension;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::response::Response;
use tower_http::trace::TraceLayer;

pub mod readiness;

use super::routes;
use super::{ApiState, endpoints, rate_limits};
use crate::api::json_body;
use crate::observability::RedactedHttpMakeSpan;
use readiness::RuntimeProbes;

/// Build the application router with no extra readiness probes (storage read
/// only, as in tests and tools).
pub fn build_router(state: ApiState) -> Router {
    build_router_with_probes(state, RuntimeProbes::new())
}

/// Build the application router with the runtime's readiness checks and
/// metric gauges (see [`readiness`]).
pub fn build_router_with_probes(state: ApiState, probes: RuntimeProbes) -> Router {
    let max_body = state.max_body_size;
    let limits = rate_limits::RateLimitState::new(
        state.trusted_proxies.clone(),
        state.rate_limit_multiplier,
    );
    let ip_block_state = state.clone();
    let invites = state.invites.clone();
    let mail_env = state.mail_env.clone();
    Router::new()
        .merge(routes::routes_health::router())
        .merge(routes::routes_oauth::router())
        .merge(routes::routes_instance::router())
        .merge(routes::routes_accounts::router())
        .merge(routes::routes_relationships::router())
        .merge(routes::routes_collections::router())
        .merge(routes::routes_statuses::router())
        .merge(routes::routes_misc::router())
        .merge(routes::routes_admin::router())
        .merge(routes::routes_timelines::router())
        .merge(routes::routes_media::router())
        .merge(routes::routes_notifications::router())
        .merge(routes::routes_lists_filters::router())
        // Federation (Phase 6): WebFinger, NodeInfo, the actor document and the
        // inboxes. The crate's middleware sits on these routes only.
        .merge(crate::activitypub::http::routes::<ApiState>(
            state.federation.clone(),
        ))
        .layer(Extension(probes))
        .layer(Extension(limits.clone()))
        .layer(Extension(invites))
        .layer(Extension(mail_env))
        .with_state(state)
        // Malformed JSON is refused before any handler runs (Rails parses
        // params before the action); this sits inside the rate limiter, which
        // runs first, as Rack::Attack does upstream.
        .layer(middleware::from_fn_with_state(
            ip_block_state,
            endpoints::admin_infra::ip_block_middleware,
        ))
        .layer(middleware::from_fn(json_body::guard))
        .layer(middleware::from_fn_with_state(
            limits,
            rate_limits::middleware,
        ))
        .layer(middleware::from_fn(vary_authorization))
        .layer(DefaultBodyLimit::max(max_body))
        // The tracing span records method + path only, never the query string:
        // confirmation/reset tokens travel in `?confirmation_token=` /
        // `?reset_password_token=`, and the default `new_for_http` span would
        // log the full URI at debug level.
        .layer(TraceLayer::new_for_http().make_span_with(RedactedHttpMakeSpan::default()))
}

/// `Vary: Authorization` on API responses (Mastodon's `vary_by
/// 'Authorization'` on every API controller), so shared caches never mix
/// personalized and anonymous payloads.
async fn vary_authorization(request: axum::extract::Request, next: middleware::Next) -> Response {
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
