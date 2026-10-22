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
//! and the processing lifecycle live in `application::media`. Two resource
//! rules live here because they are about the HTTP edge:
//!
//! - uploads stream the file part into a capped spool file (the configured
//!   `max_file_size`, independent of the whole-body limit) and processing
//!   runs on the bounded blocking pool, never on an executor thread;
//! - the public file route serves only published media keys, streams from
//!   disk and honors single `Range` requests.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::multipart::Field;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::application::media;
use crate::application::ports::{BlobStore, Store};
use crate::domain::{Error as DomainError, Id, MediaAttachment, MediaState};
use crate::media::public_media::{self, PublicMediaKind, RangeOutcome};
use crate::media::upload_gate;
use crate::media::upload_spool::{SNIFF_PREFIX_BYTES, SpoolError, UploadSpool};

/// Write scopes for every media route (Mastodon:
/// `doorkeeper_authorize! :write, :'write:media'`).
const MEDIA_SCOPES: &[&str] = &["write", "write:media"];

/// Longest accepted non-file form field (`description` is capped at 10 000
/// characters, at most four bytes each, plus slack).
const MAX_TEXT_FIELD_BYTES: u64 = 64 * 1024;

/// Bytes read per chunk when streaming a file response.
const SERVE_CHUNK_BYTES: usize = 64 * 1024;

/// `POST /api/v1/media` — synchronous upload (200 + processed attachment).
pub async fn create(State(state): State<ApiState>, auth: Auth, request: Request) -> Response {
    upload(&state, &auth, request, false).await
}

/// `POST /api/v2/media` — delayed processing for larger formats (202).
pub async fn create_v2(State(state): State<ApiState>, auth: Auth, request: Request) -> Response {
    upload(&state, &auth, request, true).await
}

/// `GET /api/v1/media/:id` — 206 while unprocessed.
pub async fn show(State(state): State<ApiState>, auth: Auth, Path(raw): Path<String>) -> Response {
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
    let (description, focus, thumbnail, license) =
        match update_fields(&state, request, state.server.max_file_size).await {
            Ok(fields) => fields,
            Err(err) => return err.into_response(),
        };
    let store = state.context.store.clone();
    let blobs = state.context.blobs.clone();
    let now_ms = state.context.now_ms();
    // Core extension CORE-L: raw license param; blank clears, absent leaves
    // unchanged; non-numeric is a 422 (the use case 422s out-of-enum, and
    // ignores everything under `off`).
    let license = match license {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => match raw.parse::<i64>() {
            Ok(value) => Some(Some(value)),
            Err(_) => {
                return ApiError::unprocessable(crate::domain::license::ERR_LICENSE_INVALID)
                    .into_response();
            }
        },
    };
    // Owned policy snapshots: the gate closure is `'static`, so `state`
    // must not be borrowed from inside it (nor used after the move).
    let license_policy = state.server.license_policy.clone();
    let alt_text_policy = state.server.alt_text_policy.clone();
    let outcome = upload_gate::media_work_gate()
        .run(move || {
            media::update_media(
                &*store,
                &*blobs,
                account_id,
                id,
                description.as_deref(),
                focus.as_deref(),
                thumbnail.as_deref(),
                now_ms,
                &license_policy,
                license,
                &alt_text_policy,
            )
        })
        .await
        .and_then(|inner| inner);
    match outcome {
        Ok(media) => {
            // Core extension CORE-T: nag header on success when enabled.
            let attachments = [(media.r#type.clone(), media.description.clone())];
            crate::api::mastodon::with_alt_text_nag(
                media_response(&state, media),
                &state.server.alt_text_policy,
                &attachments,
            )
        }
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

// ---------------------------------------------------------------------------
// Public file route.
// ---------------------------------------------------------------------------

/// Bytes located for a public request.
enum Located {
    /// A regular file under a streaming root, streamed from disk.
    File { file: tokio::fs::File, len: u64 },
    /// Bytes read through a rootless handle (in-memory test doubles only).
    Bytes(Vec<u8>),
}

impl Located {
    fn len(&self) -> u64 {
        match self {
            Located::File { len, .. } => *len,
            Located::Bytes(bytes) => bytes.len() as u64,
        }
    }
}

/// `GET /system/media_attachments/files/{*path}` — serve published media.
///
/// Only published key shapes are served (`public_media::classify_key`):
/// guessed names, traversal, backslashes, symlink paths, staging siblings
/// (`.name.part-*`, `tmp/*.part`), transcode scratch (`derived.*`) and other
/// namespaces are all a plain 404 before the filesystem is consulted. Bytes
/// stream from disk with `Accept-Ranges: bytes`; a single `Range` is
/// answered with `206`. A cached remote attachment whose bytes were evicted
/// answers 404 and queues one refetch; an evicted remote preview is rebuilt
/// from the cached original.
pub async fn serve_file(
    State(state): State<ApiState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(kind) = public_media::classify_key(&path) else {
        return ApiError::not_found().into_response();
    };
    let located = match locate(&state, &path).await {
        Ok(Some(located)) => Some(located),
        Ok(None) => recover_missing(&state, kind, &path).await,
        Err(DomainError::InvalidInput(_)) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    match located {
        Some(located) => respond(located, &headers).await,
        None => ApiError::not_found().into_response(),
    }
}

/// Find the bytes for `key`: every streaming root first (durable, then the
/// cache pool — local keys and pre-split durable `remote/` shadows stream
/// from disk, and cache-pool remotes stream from disk too), then the
/// pool-routing store for rootless doubles only.
///
/// `Located::File` streams from disk and seeks for `Range`; `Located::Bytes`
/// is the buffered fallback for handles with no filesystem root at all
/// (in-memory test doubles): it holds the whole blob in memory. `get()` is
/// never called for a key that exists as a file in either pool.
async fn locate(state: &ApiState, key: &str) -> Result<Option<Located>, DomainError> {
    let blobs = state.context.blobs.clone();
    let roots: Vec<PathBuf> = stream_roots(&blobs);
    let had_roots: bool = !roots.is_empty();
    for root in roots {
        let key_owned: String = key.to_string();
        let opened = tokio::task::spawn_blocking(move || {
            crate::media::blob_store::open_read_in_root(&root, &key_owned)
        })
        .await
        .map_err(|err| DomainError::Internal(format!("open blob task: {err}")))??;
        if let Some(file) = opened {
            let len: u64 = file
                .metadata()
                .map_err(|err| DomainError::Storage(format!("stat blob: {err}")))?
                .len();
            return Ok(Some(Located::File {
                file: tokio::fs::File::from_std(file),
                len,
            }));
        }
    }
    if !had_roots {
        let key_owned: String = key.to_string();
        let bytes = tokio::task::spawn_blocking(move || blobs.get(&key_owned))
            .await
            .map_err(|err| DomainError::Internal(format!("read blob task: {err}")))??;
        return Ok(bytes.map(Located::Bytes));
    }
    Ok(None)
}

/// Filesystem roots served by streaming, durable first.
///
/// The durable root streams local bytes and legacy durable shadows; the
/// cache root streams cache-pool remotes (durable stays first so legacy
/// shadows keep resolving to the same bytes). Handles with no filesystem
/// root (in-memory doubles) yield no roots and fall back to the buffered
/// `get()` in [`locate`].
fn stream_roots(blobs: &Arc<dyn BlobStore>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = blobs.root_path().into_iter().collect();
    roots.extend(blobs.cache_root_path());
    roots
}

/// Heal a missing cache entry: queue the single-flight refetch of a missing
/// original, or rebuild a missing remote preview from the cached original.
async fn recover_missing(state: &ApiState, kind: PublicMediaKind, key: &str) -> Option<Located> {
    match kind {
        PublicMediaKind::Local => None,
        PublicMediaKind::RemoteCache { media_id } => {
            let store = state.context.store.clone();
            let now_ms = state.context.now_ms();
            let queued = tokio::task::spawn_blocking(move || {
                crate::media::cache_maintenance::enqueue_fetch_on_miss(
                    &*store,
                    Id::new(media_id),
                    now_ms,
                )
            })
            .await;
            if let Ok(Err(err)) = queued {
                tracing::warn!(media = media_id, error = %err, "cache-miss refetch not queued");
            }
            None
        }
        PublicMediaKind::RemotePreview { media_id } => {
            let store = state.context.store.clone();
            let blobs = state.context.blobs.clone();
            let owned: String = key.to_string();
            let rebuilt = upload_gate::media_work_gate()
                .run(move || {
                    media::regenerate_remote_preview(&*store, &*blobs, Id::new(media_id), &owned)
                })
                .await
                .and_then(|inner| inner);
            if !matches!(rebuilt, Ok(true)) {
                return None;
            }
            locate(state, key).await.ok().flatten()
        }
    }
}

/// Build the response for located bytes: type from the leading bytes,
/// single-range handling, streamed body.
async fn respond(located: Located, request_headers: &HeaderMap) -> Response {
    let total: u64 = located.len();
    let range_header: Option<&str> = request_headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let outcome: RangeOutcome = public_media::evaluate_range(range_header, total);

    let (content_type, body, status, content_range, length): (
        &'static str,
        Body,
        StatusCode,
        Option<String>,
        u64,
    ) = match located {
        Located::Bytes(bytes) => {
            let content_type = sniff_content_type(&bytes[..bytes.len().min(SNIFF_PREFIX_BYTES)]);
            match outcome {
                RangeOutcome::Unsatisfiable => {
                    return unsatisfiable(total, content_type);
                }
                RangeOutcome::Partial { start, end } => {
                    let slice: Vec<u8> = bytes[start as usize..=end as usize].to_vec();
                    (
                        content_type,
                        Body::from(slice),
                        StatusCode::PARTIAL_CONTENT,
                        Some(format!("bytes {start}-{end}/{total}")),
                        end - start + 1,
                    )
                }
                RangeOutcome::Full => {
                    (content_type, Body::from(bytes), StatusCode::OK, None, total)
                }
            }
        }
        Located::File { mut file, .. } => {
            let mut prefix: Vec<u8> = Vec::new();
            if let Err(err) = (&mut file)
                .take(SNIFF_PREFIX_BYTES as u64)
                .read_to_end(&mut prefix)
                .await
            {
                tracing::warn!(error = %err, "media prefix read failed");
                return ApiError::internal().into_response();
            }
            let content_type = sniff_content_type(&prefix);
            let (start, length, status, content_range) = match outcome {
                RangeOutcome::Unsatisfiable => return unsatisfiable(total, content_type),
                RangeOutcome::Partial { start, end } => (
                    start,
                    end - start + 1,
                    StatusCode::PARTIAL_CONTENT,
                    Some(format!("bytes {start}-{end}/{total}")),
                ),
                RangeOutcome::Full => (0, total, StatusCode::OK, None),
            };
            if let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await {
                tracing::warn!(error = %err, "media seek failed");
                return ApiError::internal().into_response();
            }
            (
                content_type,
                Body::from_stream(file_stream(file, length)),
                status,
                content_range,
                length,
            )
        }
    };

    let mut response: Response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=86400, public"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; form-action 'none'"),
    );
    if let Some(content_range) = content_range
        && let Ok(value) = HeaderValue::from_str(&content_range)
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    response
}

/// 416 with the `Content-Range: bytes */len` the client needs to retry.
fn unsatisfiable(total: u64, content_type: &'static str) -> Response {
    let mut response: Response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
        headers.insert(header::CONTENT_RANGE, value);
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
}

/// The content type is decided by the bytes themselves, exactly as at upload
/// time.
fn sniff_content_type(prefix: &[u8]) -> &'static str {
    crate::media::ingest::sniff(prefix)
        .map(|sniffed| sniffed.mime_type)
        .unwrap_or("application/octet-stream")
}

/// Stream `remaining` bytes of `file` in fixed-size chunks. Dropping the
/// stream (client disconnect) closes the file.
fn file_stream(
    file: tokio::fs::File,
    remaining: u64,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold((file, remaining), |(mut file, remaining)| async move {
        if remaining == 0 {
            return None;
        }
        let want: usize = remaining.min(SERVE_CHUNK_BYTES as u64) as usize;
        let mut chunk: Vec<u8> = vec![0u8; want];
        match file.read(&mut chunk).await {
            Ok(0) => None,
            Ok(read) => {
                chunk.truncate(read);
                Some((Ok(Bytes::from(chunk)), (file, remaining - read as u64)))
            }
            Err(err) => Some((Err(err), (file, 0))),
        }
    })
}

// ---------------------------------------------------------------------------
// Upload plumbing.
// ---------------------------------------------------------------------------

/// Multipart fields of an upload request; absent fields stay `None` and a
/// non-multipart body yields the defaults (→ the blank-file 422). The file
/// part is a spool file, not a buffer: dropping the fields (error, client
/// disconnect) removes it.
#[derive(Debug, Default)]
struct UploadFields {
    file: Option<UploadSpool>,
    thumbnail: Option<Vec<u8>>,
    description: Option<String>,
    focus: Option<String>,
    /// Core extension CORE-L: raw `license` param (validated in the use
    /// case; ignored under `off`).
    license: Option<String>,
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
    // The per-file cap is the configured `max_file_size`; the whole-body
    // limit (`max_body_size`) is enforced separately by the router so
    // multipart overhead and metadata fields never weaken the file limit.
    let max_file_size: u64 = state.server.max_file_size;
    let fields = match upload_fields(state, request, max_file_size).await {
        Ok(fields) => fields,
        Err(err) => return err.into_response(),
    };
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
    let store = state.context.store.clone();
    let blobs = state.context.blobs.clone();
    let now_ms = state.context.now_ms();
    // Core extension CORE-L: raw license param; the use case validates
    // (422 on out-of-enum) unless the policy is `off` (ignored entirely).
    let license = match fields.license.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match raw.parse::<i64>() {
            Ok(value) => Some(value),
            Err(_) => {
                return crate::api::mastodon::errors::ApiError::unprocessable(
                    crate::domain::license::ERR_LICENSE_INVALID,
                )
                .into_response();
            }
        },
    };
    // Owned policy snapshots: the gate closure is `'static`, so the
    // borrowed `state` must not be captured by it.
    let license_policy = state.server.license_policy.clone();
    let alt_text_policy = state.server.alt_text_policy.clone();
    // Decode, resize and FFmpeg run on the bounded blocking pool; the slot
    // is awaited without holding a thread.
    let outcome = upload_gate::media_work_gate()
        .run(move || {
            let mut fields = fields;
            let thumbnail: Option<Vec<u8>> = fields.thumbnail.take();
            let description: String = fields.description.take().unwrap_or_default();
            let focus: Option<String> = fields.focus.take();
            match fields.file.as_mut() {
                Some(file) => media::upload_media_spooled(
                    &*store,
                    &*blobs,
                    now_ms,
                    account_id,
                    file,
                    thumbnail.as_deref(),
                    &description,
                    focus.as_deref(),
                    delay,
                    max_file_size,
                    transcode.as_ref(),
                    &license_policy,
                    license,
                    &alt_text_policy,
                ),
                None => media::upload_media_with_config(
                    &*store,
                    &*blobs,
                    now_ms,
                    account_id,
                    &[],
                    thumbnail.as_deref(),
                    &description,
                    focus.as_deref(),
                    delay,
                    max_file_size,
                    transcode.as_ref(),
                    &license_policy,
                    license,
                    &alt_text_policy,
                ),
            }
        })
        .await
        .and_then(|inner| inner);
    match outcome {
        Ok(media) => {
            let status = if delay && media.state != MediaState::Ready {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            let response = (
                status,
                axum::Json(dto::media_attachment_json(&state.server, &media)),
            )
                .into_response();
            // Core extension CORE-T: nag header on success when enabled.
            crate::api::mastodon::with_alt_text_nag(
                response,
                &state.server.alt_text_policy,
                &[(media.r#type.clone(), media.description.clone())],
            )
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

/// The pinned 422 for a part over the configured file cap.
fn spool_error(err: SpoolError) -> ApiError {
    match err {
        SpoolError::TooLarge { limit } => ApiError::from(media::file_too_large_error(limit)),
        SpoolError::Storage(err) => ApiError::from(err),
    }
}

fn multipart_error(err: axum::extract::multipart::MultipartError) -> ApiError {
    ApiError::bad_request(format!("Invalid multipart body: {err}"))
}

/// Parse the upload fields of a `multipart/form-data` body (`file`,
/// `thumbnail`, `description`, `focus` — the declared file content type is
/// never trusted). The file part streams into a spool capped at
/// `max_file_size`; the other parts have their own small caps.
async fn upload_fields(
    state: &ApiState,
    request: Request,
    max_file_size: u64,
) -> ApiResult<UploadFields> {
    if !is_multipart(&request) {
        return Ok(UploadFields::default());
    }
    let mut multipart = multipart(state, request).await?;
    let mut fields = UploadFields::default();
    while let Some(field) = next_field(&mut multipart).await? {
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("file") => fields.file = Some(spool_field(state, field, max_file_size).await?),
            Some("thumbnail") => {
                fields.thumbnail = Some(capped_bytes(field, max_file_size, FieldKind::File).await?);
            }
            Some("description") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                fields.description = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("focus") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                fields.focus = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("license") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                fields.license = Some(String::from_utf8_lossy(&value).into_owned());
            }
            _ => {}
        }
    }
    Ok(fields)
}

/// Parse the update fields: multipart (`thumbnail`/`description`/`focus`/`license`;
/// a `file` part is ignored — only the permitted params apply) or
/// form-encoded (`description`/`focus`/`license`). The license element is
/// `Some(Some(raw))` when present, `Some(None)` when present-but-blank
/// (clears), `None` when absent (unchanged).
async fn update_fields(
    state: &ApiState,
    request: Request,
    max_file_size: u64,
) -> ApiResult<(
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Option<Option<String>>,
)> {
    if !is_multipart(&request) {
        let LenientForm(pairs) = LenientForm::from_request(request, state)
            .await
            .unwrap_or_else(|never| match never {});
        return Ok((
            forms::get(&pairs, "description").map(str::to_string),
            forms::get(&pairs, "focus").map(str::to_string),
            None,
            forms::get(&pairs, "license").map(|raw| {
                let trimmed = raw.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }),
        ));
    }
    let mut multipart = multipart(state, request).await?;
    let mut description = None;
    let mut focus = None;
    let mut thumbnail = None;
    let mut license: Option<Option<String>> = None;
    while let Some(field) = next_field(&mut multipart).await? {
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("thumbnail") => {
                thumbnail = Some(capped_bytes(field, max_file_size, FieldKind::File).await?);
            }
            Some("description") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                description = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("focus") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                focus = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("license") => {
                let value = capped_bytes(field, MAX_TEXT_FIELD_BYTES, FieldKind::Text).await?;
                let raw = String::from_utf8_lossy(&value).trim().to_string();
                license = Some(if raw.is_empty() { None } else { Some(raw) });
            }
            _ => {}
        }
    }
    Ok((description, focus, thumbnail, license))
}

fn is_multipart(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"))
}

async fn multipart(state: &ApiState, request: Request) -> ApiResult<Multipart> {
    // Pace the body one frame per task wake before parsing: multer's buffer
    // drains every immediately-ready frame in one go, so a fast sender (or
    // an always-ready test stream) would make the whole part resident before
    // the spool cap trips. Pacing forces incremental delivery, so an
    // oversized part aborts after roughly the file cap instead of after the
    // whole body. Costs one extra wake per frame on upload paths only.
    let (parts, body) = request.into_parts();
    let paced = Body::from_stream(PacedBody::new(body.into_data_stream()));
    let request = Request::from_parts(parts, paced);
    Multipart::from_request(request, state)
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))
}

/// A body-frame stream that yields `Pending` (after re-waking) between
/// frames, so a greedy downstream parser cannot drain an always-ready
/// stream in a single poll. See [`multipart`].
struct PacedBody<S> {
    inner: std::pin::Pin<Box<S>>,
    pause: bool,
}

impl<S> PacedBody<S> {
    fn new(stream: S) -> Self {
        Self {
            inner: Box::pin(stream),
            pause: false,
        }
    }
}

impl<S, Ok, Err> futures_util::stream::Stream for PacedBody<S>
where
    S: futures_util::stream::Stream<Item = Result<Ok, Err>> + Send,
{
    type Item = Result<Ok, Err>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.pause {
            self.pause = false;
            cx.waker().wake_by_ref();
            return std::task::Poll::Pending;
        }
        match self.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                self.pause = true;
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }
}

async fn next_field(multipart: &mut Multipart) -> ApiResult<Option<Field<'_>>> {
    multipart.next_field().await.map_err(multipart_error)
}

/// Stream one file part into a capped spool file. The cap is enforced as the
/// bytes arrive, so an oversized part is rejected before it is resident, and
/// every failure path drops the spool (which deletes its file).
async fn spool_field(state: &ApiState, mut field: Field<'_>, limit: u64) -> ApiResult<UploadSpool> {
    let root = state.context.blobs.root_path();
    let mut spool = UploadSpool::create(root.as_deref(), limit)
        .await
        .map_err(spool_error)?;
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        spool.write_chunk(&chunk).await.map_err(spool_error)?;
    }
    spool.finish().await.map_err(spool_error)?;
    Ok(spool)
}

/// Which message an oversized small part gets.
#[derive(Debug, Clone, Copy)]
enum FieldKind {
    /// An image part: Mastodon's file-size message.
    File,
    /// A text part: a plain validation error.
    Text,
}

/// Collect a small part in memory, failing once it passes `limit` bytes.
async fn capped_bytes(mut field: Field<'_>, limit: u64, kind: FieldKind) -> ApiResult<Vec<u8>> {
    let mut collected: Vec<u8> = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if (collected.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(match kind {
                FieldKind::File => ApiError::from(media::file_too_large_error(limit)),
                FieldKind::Text => ApiError::unprocessable("Validation failed: field is too long"),
            });
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected)
}
