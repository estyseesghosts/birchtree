//! Media use cases (Phase 4): upload, the processing lifecycle, updates,
//! deletion and the wire-facing validation that mirrors Mastodon's
//! `MediaAttachment` model + `Api::V1::MediaController`.
//!
//! Handlers stay thin: every rule here (content sniffing, size limits,
//! description/focus/thumbnail validation, the inline-vs-delayed processing
//! decision, the unattached-only delete) is exercised through the ports
//! exactly as the REST adapter would drive them.

use crate::application::ports::{
    BlobStore, MediaPatch, MediaProcessingResult, NewMediaUpload, Store,
};
use crate::domain::{Error, FocusPoint, Id, MediaAttachment, MediaState, Result};
use crate::media::ingest::{self, Sniffed};

/// Mastodon's `file_type_error`: the attachment cannot be decoded.
pub const FILE_TYPE_ERROR: &str = "File type of uploaded media could not be verified";
/// Mastodon's `processing_error`: any other synchronous processing failure,
/// and the body rendered for a `Failed` attachment on show/update.
pub const THUMBNAIL_ERROR: &str = "Error processing thumbnail for uploaded media";
/// Mastodon's `in_usage_error`: deleting an attachment a status references.
pub const IN_USAGE_ERROR: &str = "Media attachment is currently used by a status";
/// Reason recorded on a `Failed` attachment whose bytes could not be
/// identified or decoded (an "unsupported media type" style reason).
pub const UNSUPPORTED_REASON: &str = "unsupported media type";

const ERR_BLANK_FILE: &str = "Validation failed: File can't be blank";
const ERR_CONTENT_TYPE: &str = "Validation failed: File content type is invalid, File is invalid";
const ERR_DESCRIPTION_TOO_LONG: &str =
    "Validation failed: Description is too long (maximum is 10000 characters)";
const ERR_THUMBNAIL_BLANK: &str = "Validation failed: Thumbnail must be blank";
const ERR_THUMBNAIL_INVALID: &str = "Validation failed: Thumbnail is invalid";
const ERR_FOCUS_INVALID: &str = "Validation failed: Focus is invalid";

/// Mastodon's `MediaAttachment::MAX_DESCRIPTION_LENGTH`.
const MAX_DESCRIPTION_LENGTH: usize = 10_000;

// ---------------------------------------------------------------------------
// Blob key layout.
// ---------------------------------------------------------------------------
//
// Mastodon shards attachment bytes by the attachment id
// (`000/000/<id>/original/<hex>.<ext>`), but bytes must be staged BEFORE the
// metadata row commits (`birchtree.md` §5 — file storage cannot join the
// `redb` transaction) and the id is minted by the store inside that commit.
// The key therefore cannot carry the id. Instead each upload mints one random
// 16-hex name and every blob of that attachment lives in the same shard:
//
//   `{aa}/{bb}/{random16}/original.{ext}`    the uploaded bytes
//   `{aa}/{bb}/{random16}/small.{ext}`       generated image preview
//   `{aa}/{bb}/{random16}/thumbnail.{ext}`   operator-supplied cover image
//
// where `aa`/`bb` are the random name's first two byte-hex pairs — 65 536
// shards with the same spread an id partition would give. `dto::media_file_url`
// serves `file_key` verbatim under `/system/media_attachments/files/`, so any
// traversal-free relative key works (and `FsBlobStore` rejects traversal).

/// A fresh `{aa}/{bb}/{random16}` shard for one attachment.
fn new_shard() -> String {
    let hex = crate::application::crypto::random_token_hex(8);
    format!("{}/{}/{}", &hex[0..2], &hex[2..4], hex)
}

/// `{shard}/{style}.{ext}`.
fn shard_key(shard: &str, style: &str, ext: &str) -> String {
    format!("{shard}/{style}.{ext}")
}

/// A sibling key in the same shard as `file_key` (`.../{style}.{ext}`), or
/// `None` when the key carries no shard (e.g. an empty placeholder).
fn sibling_key(file_key: &str, style: &str, ext: &str) -> Option<String> {
    let (dir, _) = file_key.rsplit_once('/')?;
    if dir.is_empty() {
        return None;
    }
    Some(format!("{dir}/{style}.{ext}"))
}

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// Rails' `number_to_human_size` (binary units, 3 significant digits,
/// insignificant zeros stripped): `16777216 -> "16 MB"`, `1024 -> "1 KB"`.
/// Used in Mastodon's pinned size message.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let digits_before = (value.log10().floor().max(0.0) as usize) + 1;
    let precision = 3usize.saturating_sub(digits_before);
    let mut formatted = format!("{value:.precision$}");
    if formatted.contains('.') {
        while formatted.ends_with('0') {
            formatted.pop();
        }
        if formatted.ends_with('.') {
            formatted.pop();
        }
    }
    format!("{formatted} {}", UNITS[unit])
}

/// Mastodon's file-size 422 body for a `limit` that was exceeded.
fn file_too_large_error(limit: u64) -> Error {
    let human = human_size(limit);
    Error::InvalidInput(format!(
        "Validation failed: File must be less than {human}, File file size must be less than {human}"
    ))
}

/// Parse Mastodon's `focus` param (`"x,y"`, both floats). Blank is ignored
/// (returns `None`); a non-numeric value is a validation error — Ruby's
/// `Float()` in the pinned model accepts only numeric pairs, and a
/// non-numeric focus would otherwise silently become `(0.0, 0.0)` (documented
/// deviation: we reject it with Mastodon's `Focus is invalid` instead of
/// coercing).
pub fn parse_focus(value: &str) -> Result<Option<FocusPoint>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let invalid = || Error::InvalidInput(ERR_FOCUS_INVALID.into());
    let (x, y) = trimmed.split_once(',').ok_or_else(invalid)?;
    let x = x.trim().parse::<f32>().map_err(|_| invalid())?;
    let y = y.trim().parse::<f32>().map_err(|_| invalid())?;
    if !x.is_finite() || !y.is_finite() {
        return Err(invalid());
    }
    Ok(Some(FocusPoint { x, y }))
}

// ---------------------------------------------------------------------------
// Upload.
// ---------------------------------------------------------------------------

/// `POST /api/v1/media` (+ v2): stage the bytes, commit the metadata row and
/// the `ProcessMedia` job, then finish processing inline unless `delay` asks
/// for Mastodon's larger-format deferral (`delay_processing?`: video/audio on
/// the v2 endpoint stay `Pending` and answer 202 until the job runs).
///
/// `file_bytes` is classified by its magic bytes — the declared multipart
/// content type is never trusted. `max_file_size` is the configured cap
/// (images AND video/audio alike; Mastodon's separate 99 MB video limit has
/// no second knob in our config).
#[allow(clippy::too_many_arguments)]
pub fn upload_media(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    _server_public_url: &str,
    now_ms: u64,
    account_id: Id,
    file_bytes: &[u8],
    thumbnail_bytes: Option<&[u8]>,
    description: &str,
    focus: Option<&str>,
    delay: bool,
    max_file_size: u64,
    license_policy: &str,
    license: Option<i64>,
) -> Result<MediaAttachment> {
    stage_and_finish_upload(
        store,
        blobs,
        now_ms,
        account_id,
        file_bytes,
        thumbnail_bytes,
        description,
        focus,
        delay,
        max_file_size,
        None,
        license_policy,
        license,
    )
}

/// Config-aware upload (Phase 7.1): when `config` is `Some` and enabled,
/// formats that need a transcode defer on `delay` (v2 → 202) and transcode
/// inline otherwise (v1 → 200). With `None` (or disabled) the legacy
/// lifecycle is kept exactly.
pub fn upload_media_with_config(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    now_ms: u64,
    account_id: Id,
    file_bytes: &[u8],
    thumbnail_bytes: Option<&[u8]>,
    description: &str,
    focus: Option<&str>,
    delay: bool,
    max_file_size: u64,
    config: Option<&crate::media::transcode::TranscodeConfig>,
    license_policy: &str,
    license: Option<i64>,
) -> Result<MediaAttachment> {
    stage_and_finish_upload(
        store,
        blobs,
        now_ms,
        account_id,
        file_bytes,
        thumbnail_bytes,
        description,
        focus,
        delay,
        max_file_size,
        config,
        license_policy,
        license,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_and_finish_upload(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    // Kept for signature symmetry with the other media use cases: attachment
    // URLs are rendered by `dto::media_file_url` from the committed
    // `file_key`, so the base URL is not needed while staging.
    now_ms: u64,
    account_id: Id,
    file_bytes: &[u8],
    thumbnail_bytes: Option<&[u8]>,
    description: &str,
    focus: Option<&str>,
    delay: bool,
    max_file_size: u64,
    config: Option<&crate::media::transcode::TranscodeConfig>,
    license_policy: &str,
    license: Option<i64>,
) -> Result<MediaAttachment> {
    // 1. A missing/blank file param.
    if file_bytes.is_empty() {
        return Err(Error::InvalidInput(ERR_BLANK_FILE.into()));
    }
    // 2. Content sniffed from bytes (never the declared type).
    let sniffed = sniff_accepted(file_bytes).ok_or_else(|| {
        Error::InvalidInput(ERR_CONTENT_TYPE.into())
    })?;
    // 3. Size limit.
    if file_bytes.len() as u64 > max_file_size {
        return Err(file_too_large_error(max_file_size));
    }
    // 4. Description length.
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err(Error::InvalidInput(ERR_DESCRIPTION_TOO_LONG.into()));
    }
    // 5. Focus.
    let focus = parse_focus(focus.unwrap_or_default())?;
    // 5b. Core extension CORE-L: the license rides on the attachment row.
    // Under `off` the param is accepted-and-ignored entirely (not even
    // parsed, so garbage values are ignored too); otherwise an out-of-enum
    // value is a 422 and the upload itself never gates on licenses.
    let license = if license_policy == "off" {
        None
    } else {
        crate::domain::license::parse_license(license)?
    };
    // 6. Thumbnail: only video/audio may carry one (Mastodon's
    //    `validates :thumbnail, absence: true, if: !audio_or_video?`).
    let thumbnail = match thumbnail_bytes {
        None | Some(&[]) => None,
        Some(bytes) => {
            if !ingest::is_large_format(sniffed) {
                return Err(Error::InvalidInput(ERR_THUMBNAIL_BLANK.into()));
            }
            if bytes.len() as u64 > max_file_size {
                return Err(file_too_large_error(max_file_size));
            }
            let thumb = sniff_accepted(bytes)
                .filter(|sniffed| ingest::is_image(*sniffed))
                .ok_or_else(|| Error::InvalidInput(ERR_THUMBNAIL_INVALID.into()))?;
            Some((bytes, thumb))
        }
    };

    // Stage bytes FIRST (blob storage cannot join the `redb` transaction).
    let shard = new_shard();
    let file_key = shard_key(&shard, "original", sniffed.extension);
    blobs.put(&file_key, file_bytes)?;
    let thumbnail_key = match &thumbnail {
        Some((bytes, thumb)) => {
            let key = shard_key(&shard, "thumbnail", thumb.extension);
            blobs.put(&key, bytes)?;
            Some(key)
        }
        None => None,
    };

    // ONE transaction: metadata row + owner index + ProcessMedia job.
    let media = store.create_media_upload(
        NewMediaUpload {
            account_id,
            r#type: sniffed.r#type.to_string(),
            mime_type: sniffed.mime_type.to_string(),
            description: description.to_string(),
            file_size: file_bytes.len() as u64,
            file_key,
            thumbnail_key,
            focus,
            delay,
            width: None,
            height: None,
            duration_ms: None,
            frame_rate: None,
            bitrate: None,
            license,
        },
        now_ms,
    )?;

    // Inline processing (v1 always; v2 only when the format needs no
    // deferred work). The committed job stays queued and is a no-op when a
    // worker replays it — processing is idempotent.
    let transcode_deferred = config.is_some_and(|c| c.enabled)
        && crate::media::transcode::needs_transcode(&media.mime_type);
    if delay && (media.larger_media_format() || transcode_deferred) {
        return Ok(media);
    }
    process_media_job_with_config(store, blobs, media.id, now_ms, config)?;
    let media = store
        .media_by_id(media.id)?
        .ok_or_else(|| Error::Internal("media row disappeared after upload".into()))?;
    if media.state == MediaState::Failed {
        // Mastodon's two `rescue` bodies: unidentifiable bytes -> 422
        // `FILE_TYPE_ERROR`; any other Paperclip failure -> 500
        // `THUMBNAIL_ERROR`.
        if media
            .error
            .as_deref()
            .is_some_and(|reason| reason.starts_with(THUMBNAIL_ERROR))
        {
            return Err(Error::Internal(THUMBNAIL_ERROR.into()));
        }
        return Err(Error::InvalidInput(FILE_TYPE_ERROR.into()));
    }
    Ok(media)
}

/// Sniff `bytes` and keep only accepted upload types.
fn sniff_accepted(bytes: &[u8]) -> Option<Sniffed> {
    ingest::sniff(bytes).filter(|sniffed| ingest::validate(*sniffed))
}

// ---------------------------------------------------------------------------
// Processing lifecycle.
// ---------------------------------------------------------------------------

/// Execute a `ProcessMedia` job: finish processing staged bytes and commit
/// the result (idempotent).
///
/// Contract: a missing row, or a row already `Ready`/`Failed`, is a no-op
/// `Ok(())`; undecodable bytes mark the row `Failed` (with an "unsupported
/// media type" style reason) and still complete the job; ONLY a missing
/// staged blob returns `Err` so the job is retried (transient storage).
pub fn process_media_job(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media_id: Id,
    now_ms: u64,
) -> Result<()> {
    process_media_job_with_config(store, blobs, media_id, now_ms, None)
}

/// Config-aware processing finish: rows whose MIME needs a transcode take
/// the ffmpeg pipeline when `config` is `Some` and enabled (Phase 7.1);
/// every other row — and every row when no config is threaded (sync drains,
/// unit tests) — takes the legacy pure-Rust finish.
pub fn process_media_job_with_config(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media_id: Id,
    now_ms: u64,
    config: Option<&crate::media::transcode::TranscodeConfig>,
) -> Result<()> {
    if let Some(media) = store.media_by_id(media_id)? {
        if !matches!(media.state, MediaState::Ready | MediaState::Failed)
            && crate::media::transcode::needs_transcode(&media.mime_type)
            && config.is_some_and(|c| c.enabled)
        {
            let config = config.expect("checked above");
            return crate::media::transcode::run_transcode_job(
                store, blobs, media_id, now_ms, config,
            );
        }
    }
    process_media_job_legacy(store, blobs, media_id, now_ms)
}

/// Legacy pure-Rust finish (pre-7.1 behavior, kept for untranscoded types
/// and for callers without a toolchain).
fn process_media_job_legacy(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media_id: Id,
    now_ms: u64,
) -> Result<()> {
    let Some(media) = store.media_by_id(media_id)? else {
        return Ok(());
    };
    if matches!(media.state, MediaState::Ready | MediaState::Failed) {
        return Ok(());
    }
    if media.file_key.is_empty() {
        // No staged bytes to process (e.g. a remote reference row): fail
        // terminally instead of retrying forever.
        mark_failed(
            store,
            &media,
            &format!("{UNSUPPORTED_REASON}: no staged bytes"),
            now_ms,
        )?;
        return Ok(());
    }
    let Some(bytes) = blobs.get(&media.file_key)? else {
        // Blob store unavailable/missing bytes: retryable.
        return Err(Error::Storage(format!(
            "media {media_id}: staged original missing"
        )));
    };
    match crate::media::processing::process(&media.mime_type, &bytes) {
        Ok(processed) => {
            // Stage the preview BEFORE committing the row that points at it.
            let mut preview_key = None;
            let mut preview_width = None;
            let mut preview_height = None;
            if let Some(preview) = processed.preview {
                let key = sibling_key(&media.file_key, "small", preview.extension)
                    .ok_or_else(|| Error::Internal("media key carries no shard".into()))?;
                blobs.put(&key, &preview.bytes)?;
                preview_key = Some(key);
                preview_width = Some(preview.width);
                preview_height = Some(preview.height);
            }
            store.mark_media_processed(
                media.id,
                MediaProcessingResult {
                    state: MediaState::Ready,
                    // The wire type was decided at upload (by content); an
                    // animated GIF stays `image` (no mp4 transcode → no
                    // `gifv`, documented deviation).
                    r#type: media.r#type.clone(),
                    blurhash: processed.blurhash,
                    preview_key,
                    preview_width,
                    preview_height,
                    width: processed.width,
                    height: processed.height,
                    duration_ms: processed.duration_ms,
                    frame_rate: None,
                    bitrate: None,
                    error: None,
                },
                now_ms,
            )?;
        }
        Err(err) => {
            tracing::warn!(media = media_id.value(), error = %err, "media processing failed");
            let reason = match &err {
                crate::media::processing::ProcessError::Unidentifiable(detail) => {
                    format!("{UNSUPPORTED_REASON}: {detail}")
                }
                crate::media::processing::ProcessError::Other(_) => THUMBNAIL_ERROR.to_string(),
            };
            mark_failed(store, &media, &reason, now_ms)?;
        }
    }
    Ok(())
}

/// Commit a `Failed` state for `media` (job still completes).
fn mark_failed(
    store: &dyn Store,
    media: &MediaAttachment,
    reason: &str,
    now_ms: u64,
) -> Result<()> {
    store.mark_media_processed(
        media.id,
        MediaProcessingResult {
            state: MediaState::Failed,
            r#type: media.r#type.clone(),
            blurhash: None,
            preview_key: None,
            preview_width: None,
            preview_height: None,
            width: None,
            height: None,
            duration_ms: None,
            frame_rate: None,
            bitrate: None,
            error: Some(reason.to_string()),
        },
        now_ms,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Read / update / delete.
// ---------------------------------------------------------------------------

/// `GET /api/v1/media/:id` — Mastodon's `set_media_attachment`: only the
/// owner's *unattached* rows are addressable, others 404. A `Failed` row
/// 422s with the processing error (`check_processing`) instead of rendering.
pub fn show_media(store: &dyn Store, account_id: Id, id: Id) -> Result<MediaAttachment> {
    let media = load_addressable_media(store, account_id, id)?;
    if media.state == MediaState::Failed {
        return Err(Error::InvalidInput(THUMBNAIL_ERROR.into()));
    }
    Ok(media)
}

/// `PUT/PATCH /api/v1/media/:id` — apply description/focus/thumbnail.
///
/// `description`/`focus`: `None` leaves the field unchanged; a present-blank
/// focus clears the point (Mastodon's `focus=` ignores blank on write, which
/// for updates drops the stored value); a present-blank description clears the
/// text. `thumbnail`: `None` unchanged, empty clears, non-empty must be an
/// image within the size cap and replaces (and deletes) the previous cover.
pub fn update_media(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    account_id: Id,
    id: Id,
    description: Option<&str>,
    focus: Option<&str>,
    thumbnail: Option<&[u8]>,
    now_ms: u64,
    license_policy: &str,
    license: Option<Option<i64>>,
) -> Result<MediaAttachment> {
    // Same addressability + `check_processing` gate as `show_media`.
    let media = load_addressable_media(store, account_id, id)?;
    if media.state == MediaState::Failed {
        return Err(Error::InvalidInput(THUMBNAIL_ERROR.into()));
    }

    let mut patch = MediaPatch::default();
    if let Some(description) = description {
        if description.chars().count() > MAX_DESCRIPTION_LENGTH {
            return Err(Error::InvalidInput(ERR_DESCRIPTION_TOO_LONG.into()));
        }
        patch.description = Some(description.to_string());
    }
    // Core extension CORE-L: present license replaces, present-blank clears,
    // absent leaves unchanged. Under `off` the param is accepted-and-ignored.
    if let Some(raw) = license
        && license_policy != "off"
    {
        let parsed = match raw {
            None => None,
            Some(value) => crate::domain::license::parse_license(Some(value))?,
        };
        patch.license = Some(parsed);
    }
    if let Some(focus) = focus {
        patch.focus = Some(parse_focus(focus)?);
    }
    let mut replaced_thumbnail: Option<String> = None;
    if let Some(thumbnail) = thumbnail {
        if thumbnail.is_empty() {
            patch.thumbnail_key = Some(None);
        } else {
            if !media.larger_media_format() {
                return Err(Error::InvalidInput(ERR_THUMBNAIL_BLANK.into()));
            }
            let thumb = sniff_accepted(thumbnail)
                .filter(|sniffed| ingest::is_image(*sniffed))
                .ok_or_else(|| Error::InvalidInput(ERR_THUMBNAIL_INVALID.into()))?;
            // Stage the new cover BEFORE the row commit; the previous cover
            // (if any) is deleted after it.
            let key = sibling_key(&media.file_key, "thumbnail", thumb.extension)
                .ok_or_else(|| Error::InvalidInput(ERR_THUMBNAIL_INVALID.into()))?;
            blobs.put(&key, thumbnail)?;
            replaced_thumbnail = media.thumbnail_key.clone();
            patch.thumbnail_key = Some(Some(key));
        }
    }

    let updated = store
        .update_media(account_id, id, patch, now_ms)?
        .ok_or_else(|| Error::NotFound("media".into()))?;
    // When the cover was replaced and the new key differs, drop the old blob
    // (best-effort: a failure leaves an orphan blob, never a broken row — the
    // row already committed).
    if let Some(old) = replaced_thumbnail
        && updated.thumbnail_key.as_deref() != Some(old.as_str())
        && let Err(err) = blobs.delete(&old)
    {
        tracing::warn!(media = id.value(), error = %err, "old media thumbnail left in place");
    }
    Ok(updated)
}

/// `DELETE /api/v1/media/:id` — only the owner's *unattached* rows; the
/// remaining bytes are removed after the row commits (a crash in between
/// leaves an orphan blob, never a dangling row).
pub fn delete_media(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    account_id: Id,
    id: Id,
) -> Result<()> {
    let media = store
        .owner_media(account_id, id)?
        .ok_or_else(|| Error::NotFound("media".into()))?;
    if media.status_id.is_some() {
        return Err(Error::InvalidInput(IN_USAGE_ERROR.into()));
    }
    store
        .delete_media(account_id, id)?
        .ok_or_else(|| Error::NotFound("media".into()))?;
    let keys = [
        Some(media.file_key),
        media.preview_key,
        media.thumbnail_key,
    ];
    for key in keys.into_iter().flatten().filter(|key| !key.is_empty()) {
        if let Err(err) = blobs.delete(&key) {
            tracing::warn!(media = id.value(), error = %err, "orphan media blob left in place");
        }
    }
    Ok(())
}

/// The owner's unattached row (`set_media_attachment`); foreign or attached
/// rows are `Record not found`.
fn load_addressable_media(
    store: &dyn Store,
    account_id: Id,
    id: Id,
) -> Result<MediaAttachment> {
    let media = store
        .owner_media(account_id, id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if media.status_id.is_some() {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(media)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::application::ports::{JobQueue, MailSender, MediaStore};
    use crate::domain::{Clock, IdGenerator, SystemClock, TimestampIdGenerator};
    use crate::mail::LoggingMailSender;
    use crate::persistence::redb::RedbStore;

    fn test_deps() -> (tempfile::TempDir, Arc<RedbStore>, crate::media::blob_store::FsBlobStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> =
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
        let store = Arc::new(
            RedbStore::open(dir.path().join("birchtree.redb"), clock, ids).expect("open store"),
        );
        let blobs = crate::media::blob_store::FsBlobStore::open(dir.path().join("media"))
            .expect("open blob store");
        (dir, store, blobs)
    }

    /// A valid 2x1 PNG (encoded with the same decoder stack the server uses).
    fn png_bytes() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([10, 20, 30, 255]),
        ));
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png).expect("png");
        cursor.into_inner()
    }

    fn mp4_bytes() -> Vec<u8> {
        let mut mp4 = vec![0u8, 0, 0, 0x18];
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0; 16]);
        mp4
    }

    #[test]
    fn human_size_formats_like_rails_number_to_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(16 * 1024 * 1024), "16 MB");
        assert_eq!(human_size(99 * 1024 * 1024), "99 MB");
        assert_eq!(human_size(1_234_567), "1.18 MB");
    }

    /// Assert an error is `InvalidInput` carrying `expected`.
    fn assert_invalid_input(err: Error, expected: &str) {
        match err {
            Error::InvalidInput(ref msg) => assert_eq!(msg, expected),
            other => panic!("expected InvalidInput({expected}), got {other:?}"),
        }
    }

    #[test]
    fn focus_parses_floats_and_rejects_garbage() {
        assert!(parse_focus("").unwrap().is_none());
        assert!(parse_focus("  ").unwrap().is_none());
        let focus = parse_focus("0.5,-0.25").unwrap().unwrap();
        assert_eq!((focus.x, focus.y), (0.5, -0.25));
        assert_invalid_input(parse_focus("abc").unwrap_err(), ERR_FOCUS_INVALID);
        assert!(parse_focus("1").is_err());
    }

    #[test]
    fn size_limit_message_reports_the_configured_cap() {
        let (_dir, store, blobs) = test_deps();
        let account_id = Id::new(1);
        // A 2 KB PNG against a 1 KB cap: the pinned "File must be less than
        // 1 KB" message (the HTTP path uses the configured 16 MB default).
        let mut big = png_bytes();
        big.extend_from_slice(&[0u8; 2048]);
        let err = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1_700_000_000_000,
            account_id,
            &big,
            None,
            "",
            None,
            false,
            1024,
            "off",
            None,
        )
        .expect_err("oversized upload rejected");
        assert_invalid_input(
            err,
            "Validation failed: File must be less than 1 KB, File file size must be less than 1 KB",
        );
    }

    #[test]
    fn blank_and_unaccepted_uploads_are_rejected_before_staging() {
        let (_dir, store, blobs) = test_deps();
        let err = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1,
            Id::new(1),
            b"",
            None,
            "",
            None,
            false,
            1024 * 1024,
            "off",
            None,
        )
        .expect_err("blank file");
        assert_invalid_input(err, ERR_BLANK_FILE);

        let err = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1,
            Id::new(1),
            b"plain text, definitely not media",
            None,
            "",
            None,
            false,
            1024 * 1024,
            "off",
            None,
        )
        .expect_err("text file");
        assert_invalid_input(err, ERR_CONTENT_TYPE);
    }

    #[test]
    fn image_upload_processes_inline_and_is_idempotent() {
        let (_dir, store, blobs) = test_deps();
        let bytes = png_bytes();
        let media = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1_700_000_000_000,
            Id::new(7),
            &bytes,
            None,
            "a description",
            Some("0.5,-0.25"),
            false,
            16 * 1024 * 1024,
            "off",
            None,
        )
        .expect("upload");
        assert_eq!(media.state, MediaState::Ready);
        assert_eq!(media.r#type, "image");
        assert_eq!(media.mime_type, "image/png");
        assert_eq!((media.width, media.height), (Some(2), Some(1)));
        assert!(media.blurhash.is_some());
        assert!(media.preview_key.is_some());
        assert_eq!(media.description, "a description");
        assert_eq!(
            (media.focus.unwrap().x, media.focus.unwrap().y),
            (0.5, -0.25)
        );
        // The original bytes are staged at the committed key.
        assert_eq!(blobs.get(&media.file_key).unwrap(), Some(bytes.clone()));

        // Replaying the ProcessMedia job is a no-op.
        crate::application::media::process_media_job(&*store, &blobs, media.id, 1_700_000_000_001)
            .expect("replay");
        let again = store.media_by_id(media.id).unwrap().unwrap();
        assert_eq!(again.state, MediaState::Ready);
        assert_eq!(again.file_key, media.file_key);
    }

    #[test]
    fn delayed_mp4_stays_pending_until_the_job_runs() {
        let (_dir, store, blobs) = test_deps();
        let bytes = mp4_bytes();
        let media = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1_700_000_000_000,
            Id::new(9),
            &bytes,
            None,
            "",
            None,
            true,
            16 * 1024 * 1024,
            "off",
            None,
        )
        .expect("upload");
        assert_eq!(media.state, MediaState::Pending);
        assert!(media.url.is_none());

        // The queued job (claimed by a worker) marks it Ready.
        let jobs = store.claim_due(1_800_000_000_000, 10).expect("claim");
        assert_eq!(jobs.len(), 1);
        let mail = LoggingMailSender;
        crate::jobs::handlers::handle(
            &jobs[0],
            &*store,
            &mail as &dyn MailSender,
            &blobs,
            &crate::application::ports::NoStreaming,
        )
            .expect("handle");
        let processed = store.media_by_id(media.id).unwrap().unwrap();
        assert_eq!(processed.state, MediaState::Ready);
        // No tkhd box in this stub container: dimensions stay None (honest).
        assert_eq!((processed.width, processed.height), (None, None));
    }

    #[test]
    fn thumbnails_are_video_audio_only() {
        let (_dir, store, blobs) = test_deps();
        let err = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1,
            Id::new(11),
            &png_bytes(),
            Some(&png_bytes()),
            "",
            None,
            false,
            16 * 1024 * 1024,
            "off",
            None,
        )
        .expect_err("thumbnail on an image");
        assert_invalid_input(err, ERR_THUMBNAIL_BLANK);
    }

    #[test]
    fn update_and_delete_round_trip() {
        let (_dir, store, blobs) = test_deps();
        let media = upload_media(
            &*store,
            &blobs,
            "https://example.social",
            1_700_000_000_000,
            Id::new(13),
            &png_bytes(),
            None,
            "",
            None,
            false,
            16 * 1024 * 1024,
            "off",
            None,
        )
        .expect("upload");

        let updated = update_media(
            &*store,
            &blobs,
            Id::new(13),
            media.id,
            Some("updated"),
            Some("0.1,0.2"),
            None,
            1_700_000_000_001,
            "off",
            None,
        )
        .expect("update");
        assert_eq!(updated.description, "updated");
        assert_eq!(
            (updated.focus.unwrap().x, updated.focus.unwrap().y),
            (0.1, 0.2)
        );

        // Foreign accounts cannot see or patch it.
        assert!(show_media(&*store, Id::new(99), media.id).is_err());
        assert!(
            update_media(&*store, &blobs, Id::new(99), media.id, Some("x"), None, None, 2, "off", None)
                .is_err()
        );

        let preview = updated.preview_key.clone().expect("preview staged");
        delete_media(&*store, &blobs, Id::new(13), media.id).expect("delete");
        assert!(store.media_by_id(media.id).unwrap().is_none());
        assert!(blobs.get(&media.file_key).unwrap().is_none());
        assert!(blobs.get(&preview).unwrap().is_none());
    }
}
