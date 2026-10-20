use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;

pub async fn index(State(_state): State<ApiState>, auth: Auth) -> Response {
    // User gate first (upstream `require_user!` runs before the campaign
    // lookup in `donation_campaigns_controller.rb:3`), so anonymous callers
    // still get 401.
    if let Err(err) = auth.0.require_user() {
        return err.into_response();
    }
    // Honest absence (P3 operator decision: build-vs-remove resolved to
    // absence). Upstream (`donation_campaigns_controller.rb:13-22` at
    // `8ff11c2b`) serves real data: a cached outbound fetch of the
    // operator-configured campaign API URL, 204 when unconfigured or when
    // the fetch yields nothing. The outbound fetch is not built here, so
    // there is never campaign data to serve; the previous unconditional 204
    // is replaced by 404 like other intentionally-absent profile routes
    // (endpoint-manifest convention: never a success-shaped stub).
    ApiError::not_found().into_response()
}
