//! Custom emoji endpoints: the public index and image serving.
//!
//! `GET /api/v1/custom_emojis` is public (no token), lists local + enabled +
//! picker-visible emoji, and is publicly cacheable for 5 minutes like
//! Mastodon's `cache_even_if_authenticated!`.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::emoji;
use crate::application::ports::Store;
use crate::domain::Error as DomainError;
use crate::media::public_media;

/// `GET /api/v1/custom_emojis`.
pub async fn index(State(state): State<ApiState>) -> Response {
    let store: &dyn Store = &*state.context.store;
    match emoji::index(store) {
        Ok(rows) => {
            let body: Vec<_> = rows
                .iter()
                .map(|row| dto::custom_emoji_json(&state.server, row, true))
                .collect();
            let mut response = axum::Json(body).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("max-age=300, public"),
            );
            response
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /system/custom_emojis/images/{*path}` — serve an emoji image.
///
/// Only published emoji key shapes are served
/// (`public_media::emoji_blob_key`): guessed names, traversal, backslashes,
/// symlink paths, staging siblings and other namespaces are all a plain 404
/// before the filesystem is consulted, exactly like the attachment route.
pub async fn serve_image(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> Response {
    let Some(key) = public_media::emoji_blob_key(&path) else {
        return ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let blobs = &*state.context.blobs;
    match blobs.get(&key) {
        Ok(Some(bytes)) => {
            let content_type = match crate::media::ingest::sniff(&bytes) {
                Some(sniffed) => sniffed.mime_type,
                None => "application/octet-stream",
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type.to_string()),
                    (
                        header::CACHE_CONTROL,
                        "max-age=86400, public".to_string(),
                    ),
                    (
                        header::X_CONTENT_TYPE_OPTIONS,
                        "nosniff".to_string(),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(None) => ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(DomainError::InvalidInput(_)) => {
            ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}
