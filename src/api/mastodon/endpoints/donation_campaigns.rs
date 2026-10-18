use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;

pub async fn index(State(_state): State<ApiState>, auth: Auth) -> Response {
    // any scope / require_user; allow anonymous? manifest says user (any scope). Require user.
    if let Err(err) = auth.0.require_user() {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
