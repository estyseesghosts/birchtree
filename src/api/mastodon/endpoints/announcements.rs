use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;

pub async fn index(State(_state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_user() {
        return err.into_response();
    }
    axum::Json(serde_json::json!([])).into_response()
}

pub async fn dismiss(State(_state): State<ApiState>, auth: Auth, _path: axum::extract::Path<String>) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn reaction_put(State(_state): State<ApiState>, auth: Auth, _path: axum::extract::Path<(String, String)>) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:favourites"]) {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn reaction_delete(State(_state): State<ApiState>, auth: Auth, _path: axum::extract::Path<(String, String)>) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:favourites"]) {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
