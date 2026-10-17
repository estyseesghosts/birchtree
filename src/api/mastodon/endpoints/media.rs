//! Media endpoints (Phase 4): v1/v2 upload, show/update/destroy, and local
//! file serving.
//!
//! Implemented by the media slice; the routes are registered already so the
//! surface never silently 404s while the handlers land.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;

/// `POST /api/v1/media` — synchronous upload (200 + processed attachment).
pub async fn create(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}

/// `POST /api/v2/media` — delayed processing for larger formats (202).
pub async fn create_v2(_state: State<ApiState>) -> Response {
    ApiError::internal().into_response()
}

/// `GET /api/v1/media/{id}` — 206 while unprocessed.
pub async fn show(_state: State<ApiState>, _path: Path<String>) -> Response {
    ApiError::internal().into_response()
}

/// `PUT/PATCH /api/v1/media/{id}`.
pub async fn update(_state: State<ApiState>, _path: Path<String>) -> Response {
    ApiError::internal().into_response()
}

/// `DELETE /api/v1/media/{id}` — 200 `{}`; 422 while attached to a status.
pub async fn destroy(_state: State<ApiState>, _path: Path<String>) -> Response {
    ApiError::internal().into_response()
}

/// `GET /system/media_attachments/files/{*path}` — serve attachment bytes.
pub async fn serve_file(_state: State<ApiState>, _path: Path<String>) -> Response {
    ApiError::internal().into_response()
}
