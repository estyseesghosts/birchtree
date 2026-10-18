use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;

pub async fn v1(State(_state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let mut resp = axum::Json(serde_json::json!([])).into_response();
    resp.headers_mut().insert(
        "Deprecation",
        HeaderValue::from_static("true"),
    );
    resp
}

pub async fn v2(State(_state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    axum::Json(serde_json::json!([])).into_response()
}

pub async fn delete_v1(State(_state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_v2(State(_state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
