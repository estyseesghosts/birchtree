//! Media endpoints (Phase 4): v1/v2 upload, show/update/destroy and local
//! file serving.
//!
//! Bodies, status codes and error bodies mirror `Api::V1::MediaController` /
//! `Api::V2::MediaController` of the pinned Mastodon source: v1 always answers
//! 200 after synchronous processing (`Paperclip` runs inside the create), v2
//! answers 202 while a larger format is still queued; show/update answer 206
//! while unprocessed and 422 `Error processing thumbnail for uploaded media`
//! when processing failed; destroy answers 200 `{}` (or 422 while the
//! attachment is used by a status). All media routes — including `show` —
//! require `write`/`write:media` (`doorkeeper_authorize!` before every
//! action), and a user token (`require_user!`).
//!
//! Handlers stay thin: sniffing, size limits, focus parsing, thumbnail rules
//! and the processing lifecycle live in `application::media`.

use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::ApiState;
use crate::application::media;
use crate::application::ports::Store;
use crate::domain::{Error as DomainError, Id, MediaAttachment, MediaState};

/// Write scopes for every media route (Mastodon:
/// `doorkeeper_authorize! :write, :'write:media'`).
const MEDIA_SCOPES: &[&str] = &["write", "write:media"];

/// `POST /api/v1/media` — synchronous upload (200 + processed attachment).
pub async fn create(State(state): State<ApiState>, auth: Auth, request: Request) -> Response {
    upload(&state, &auth, request, false).await
}

/// `POST /api/v2/media` — delayed processing for larger formats (202).
pub async fn create_v2(State(state): State<ApiState>, auth: Auth, request: Request) -> Response {
    upload(&state, &auth, request, true).await
}

/// `GET /api/v1/media/:id` — 206 while unprocessed.
pub async fn show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(MEDIA_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match media::show_media(store, account_id, id) {
        Ok(media) => media_response(&state, media),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT/PATCH /api/v1/media/:id`.
pub async fn update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    request: Request,
) -> Response {
    if let Err(err) = auth.0.require_scopes(MEDIA_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let (description, focus, thumbnail) = match update_fields(&state, request).await {
        Ok(fields) => fields,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match media::update_media(
        store,
        &*state.context.blobs,
        account_id,
        id,
        description.as_deref(),
        focus.as_deref(),
        thumbnail.as_deref(),
        state.context.now_ms(),
    ) {
        Ok(media) => media_response(&state, media),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/media/:id` — 200 `{}`; 422 while attached to a status.
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(MEDIA_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match media::delete_media(store, &*state.context.blobs, account_id, id) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /system/media_attachments/files/{*path}` — serve attachment bytes.
///
/// The URL path equals the blob key verbatim (see `dto::media_file_url`), so
/// the key is the captured path; `FsBlobStore` rejects traversal, and any
/// rejection of the key shape is a plain 404.
pub async fn serve_file(State(state): State<ApiState>, Path(path): Path<String>) -> Response {
    let blobs = &*state.context.blobs;
    match blobs.get(&path) {
        Ok(Some(bytes)) => {
            // The content type is decided by the bytes themselves, exactly as
            // at upload time.
            let content_type = crate::media::ingest::sniff(&bytes)
                .map(|sniffed| sniffed.mime_type)
                .unwrap_or("application/octet-stream");
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type.to_string()),
                    (header::CACHE_CONTROL, "max-age=86400, public".to_string()),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(None) | Err(DomainError::InvalidInput(_)) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Upload plumbing.
// ---------------------------------------------------------------------------

/// Multipart fields of an upload request; absent fields stay `None` and a
/// non-multipart body yields the defaults (→ the blank-file 422).
#[derive(Debug, Default)]
struct UploadFields {
    file: Vec<u8>,
    thumbnail: Option<Vec<u8>>,
    description: Option<String>,
    focus: Option<String>,
}

/// Shared v1/v2 upload path. `delay` enables Mastodon's larger-format
/// deferral (`delay_processing?`): video/audio return before processing with
/// 202, images still process inline.
async fn upload(state: &ApiState, auth: &Auth, request: Request, delay: bool) -> Response {
    if let Err(err) = auth.0.require_scopes(MEDIA_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let fields = match upload_fields(state, request).await {
        Ok(fields) => fields,
        Err(err) => return err.into_response(),
    };
    // `ApiState` carries no `Config`; the multipart body limit doubles as the
    // file-size cap (`Config::validate` enforces `max_file_size <=
    // max_body_size`, so this is the effective configured limit).
    let max_file_size = state.max_body_size as u64;
    // Phase 7.1: the worker-threaded toolchain, built from `ServerInfo` when
    // the operator left transcoding on. Startup validation already proved
    // the toolchain, so a failure here is a programmer error, never a
    // silent passthrough.
    let transcode = if state.server.transcoding_enabled {
        Some(crate::media::transcode::TranscodeConfig {
            enabled: true,
            ffmpeg_path: state.server.ffmpeg_path.clone(),
            wall_time_secs: state.server.transcode_wall_time_secs,
            max_output_bytes: state.server.transcode_max_output_bytes,
            max_memory_bytes: state.server.transcode_max_memory_bytes,
            max_file_size: state.server.max_file_size,
            ..crate::media::transcode::TranscodeConfig::default()
        })
    } else {
        None
    };
    let transcode_ref = transcode.as_ref();
    match media::upload_media_with_config(
        &*state.context.store,
        &*state.context.blobs,
        state.context.now_ms(),
        account_id,
        &fields.file,
        fields.thumbnail.as_deref(),
        fields.description.as_deref().unwrap_or_default(),
        fields.focus.as_deref(),
        delay,
        max_file_size,
        transcode_ref,
    ) {
        Ok(media) => {
            let status = if delay && media.state != MediaState::Ready {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (
                status,
                axum::Json(dto::media_attachment_json(&state.server, &media)),
            )
                .into_response()
        }
        Err(err) => upload_error(err).into_response(),
    }
}

/// Map an upload failure: the two synchronous Paperclip rescues keep their
/// pinned bodies, everything else follows the domain-error mapping.
fn upload_error(err: DomainError) -> ApiError {
    match err {
        DomainError::Internal(ref message) if message == media::THUMBNAIL_ERROR => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, media::THUMBNAIL_ERROR)
        }
        // `InvalidInput(FILE_TYPE_ERROR)` lands here as the pinned 422, as do
        // the "Validation failed: ..." messages.
        other => ApiError::from(other),
    }
}

/// 200 once processed, 206 `Partial Content` while queued/in progress
/// (Mastodon's `status_code_for_media_attachment`).
fn media_response(state: &ApiState, media: MediaAttachment) -> Response {
    let status = if media.not_processed() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    (
        status,
        axum::Json(dto::media_attachment_json(&state.server, &media)),
    )
        .into_response()
}

/// Parse the upload fields of a `multipart/form-data` body (`file`,
/// `thumbnail`, `description`, `focus` — the declared file content type is
/// never trusted).
async fn upload_fields(state: &ApiState, request: Request) -> ApiResult<UploadFields> {
    if !is_multipart(&request) {
        return Ok(UploadFields::default());
    }
    let mut multipart = multipart(state, request).await?;
    let mut fields = UploadFields::default();
    while let Some(field) = next_field(&mut multipart).await? {
        let name = field.name().map(str::to_string);
        let value = field_bytes(field).await?;
        match name.as_deref() {
            Some("file") => fields.file = value,
            Some("thumbnail") => fields.thumbnail = Some(value),
            Some("description") => {
                fields.description = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("focus") => fields.focus = Some(String::from_utf8_lossy(&value).into_owned()),
            _ => {}
        }
    }
    Ok(fields)
}

/// Parse the update fields: multipart (`thumbnail`/`description`/`focus`;
/// a `file` part is ignored — only the permitted params apply) or
/// form-encoded (`description`/`focus`).
async fn update_fields(
    state: &ApiState,
    request: Request,
) -> ApiResult<(Option<String>, Option<String>, Option<Vec<u8>>)> {
    if !is_multipart(&request) {
        let LenientForm(pairs) = LenientForm::from_request(request, state)
            .await
            .unwrap_or_else(|never| match never {});
        return Ok((
            forms::get(&pairs, "description").map(str::to_string),
            forms::get(&pairs, "focus").map(str::to_string),
            None,
        ));
    }
    let mut multipart = multipart(state, request).await?;
    let mut description = None;
    let mut focus = None;
    let mut thumbnail = None;
    while let Some(field) = next_field(&mut multipart).await? {
        let name = field.name().map(str::to_string);
        let value = field_bytes(field).await?;
        match name.as_deref() {
            Some("thumbnail") => thumbnail = Some(value),
            Some("description") => description = Some(String::from_utf8_lossy(&value).into_owned()),
            Some("focus") => focus = Some(String::from_utf8_lossy(&value).into_owned()),
            _ => {}
        }
    }
    Ok((description, focus, thumbnail))
}

fn is_multipart(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"))
}

async fn multipart(state: &ApiState, request: Request) -> ApiResult<Multipart> {
    Multipart::from_request(request, state).await.map_err(|err| {
        ApiError::bad_request(format!("Invalid multipart body: {err}"))
    })
}

async fn next_field(multipart: &mut Multipart) -> ApiResult<Option<axum::extract::multipart::Field<'_>>> {
    multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))
}

async fn field_bytes(field: axum::extract::multipart::Field<'_>) -> ApiResult<Vec<u8>> {
    field
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))
}
