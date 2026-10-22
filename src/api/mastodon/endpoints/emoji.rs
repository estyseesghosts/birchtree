//! Custom emoji endpoints: the public index and image serving.
//!
//! `GET /api/v1/custom_emojis` is public (no token), lists local + enabled +
//! picker-visible emoji, and is publicly cacheable for 5 minutes like
//! Mastodon's `cache_even_if_authenticated!`.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::emoji;
use crate::application::ports::Store;
use crate::domain::Error as DomainError;
use crate::domain::JobKind;
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
pub async fn serve_image(State(state): State<ApiState>, Path(path): Path<String>) -> Response {
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
                    (header::CACHE_CONTROL, "max-age=86400, public".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
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

/// `GET /system/remote/emojis/{*path}` — serve a cached remote-emoji image.
///
/// Only published remote-emoji key shapes are served
/// (`public_media::remote_emoji_blob_key`): anything else is a plain 404
/// before the filesystem is consulted. A miss whose render row survives
/// enqueues one `FetchRemoteEmoji` refetch and answers 404 (evicted bytes
/// heal; they never 500). Remote emoji are tiny (256 KiB cap), so the hit
/// path buffers like the local emoji route.
pub async fn serve_remote_image(
    State(state): State<ApiState>,
    Path(path): Path<String>,
) -> Response {
    // The route strips `/system/remote/emojis/`; the gate takes the full
    // blob key (`remote/emojis/{domain}/{shortcode}/{style}.png`).
    let full = format!("remote/emojis/{path}");
    let Some(key) = public_media::remote_emoji_blob_key(&full) else {
        return ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let store: &dyn Store = &*state.context.store;
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
                    (header::CACHE_CONTROL, "max-age=86400, public".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(None) => {
            refetch_remote_emoji(store, &key, state.context.now_ms());
            ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response()
        }
        Err(DomainError::InvalidInput(_)) => {
            ApiError::new(StatusCode::NOT_FOUND, "Not Found").into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Split a validated `remote/emojis/{domain}/{shortcode}/{style}.png` key.
/// `None` for shapes the gate already refused (unreachable in practice).
fn split_remote_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix("remote/emojis/")?;
    let (domain, rest) = rest.split_once('/')?;
    let (shortcode, _) = rest.split_once('/')?;
    Some((domain.to_string(), shortcode.to_string()))
}

/// Queue one `FetchRemoteEmoji` refetch for an evicted remote emoji, when
/// its render row still knows the origin URL. Best-effort: a missing row or
/// a missing origin URL simply leaves the 404 unhealed (never a 500).
fn refetch_remote_emoji(store: &dyn Store, key: &str, now_ms: u64) {
    let Some((domain, shortcode)) = split_remote_key(key) else {
        return;
    };
    let rows = match store.remote_emojis_by_shortcodes(&[shortcode.clone()]) {
        Ok(rows) => rows,
        Err(_) => return,
    };
    let Some(row) = rows
        .iter()
        .find(|row| row.domain == domain && !row.source_url.is_empty())
    else {
        return;
    };
    let _ = store.enqueue(
        JobKind::FetchRemoteEmoji,
        serde_json::json!({
            "shortcode": row.shortcode,
            "domain": row.domain,
            "source_url": row.source_url,
        }),
        now_ms,
    );
}
