//! Timeline endpoints (Phase 4): home, public, tag and list.
//!
//! Implemented by the timelines slice; the routes are registered already so
//! the surface never silently 404s while the handlers land.

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;

/// `GET /api/v1/timelines/home`.
pub async fn home(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}

/// `GET /api/v1/timelines/public`.
pub async fn public(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}

/// `GET /api/v1/timelines/tag/{tag}`.
pub async fn tag(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}

/// `GET /api/v1/timelines/list/{id}`.
pub async fn list(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}
