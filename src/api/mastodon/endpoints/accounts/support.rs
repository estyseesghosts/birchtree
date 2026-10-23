use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::Store;

/// Scope alternatives accepted by the write endpoints.
pub(super) const WRITE_SCOPES: &[&str] = &["write", "write:accounts"];

/// Upstream avatar/header image constraints
/// (`Account::Avatar::AVATAR_IMAGE_MIME_TYPES` /
/// `Account::Header::HEADER_IMAGE_MIME_TYPES` at the pin): still images only
/// (the probed still-image decoders minus the birchtree D4 extensions —
/// BMP/TIFF/HEIC/HEIF need the ffmpeg toolchain and upstream avatars reject
/// them), 8 MB each, descriptions capped at 150 chars.
pub(super) const PROFILE_IMAGE_MIME_TYPES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/webp"];
pub(super) const AVATAR_LIMIT: u64 = 8 * 1024 * 1024;
pub(super) const HEADER_LIMIT: u64 = 8 * 1024 * 1024;
pub(super) const MAX_IMAGE_DESCRIPTION_LEN: usize = 150;

/// Attach the caching headers Mastodon uses for token responses.
pub(super) fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

pub(super) fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Build the `CredentialAccountSerializer` body for `account_id`.
pub(super) fn credential_response(state: &ApiState, account_id: crate::domain::Id) -> Response {
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_id(account_id) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    let user = store.user_by_account_id(account_id).ok().flatten();
    let stats = store.account_stats(account_id).ok();
    axum::Json(dto::credential_account_json_with_stats(
        &state.server,
        &account,
        user.as_ref(),
        stats.as_ref(),
    ))
    .into_response()
}

/// 422 with Mastodon's `ValidationErrorFormatter` shape.
pub(super) fn validation_error(message: impl Into<String>, details: Value) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({ "error": message.into(), "details": details })),
    )
        .into_response()
}
