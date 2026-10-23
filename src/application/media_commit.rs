//! Upload persistence stages (S4 group B extraction from [`crate::application::media`]).
//!
//! Staging-then-commit helpers for the media upload use cases: stage the
//! bytes under a fresh shard ([`stage_bytes`]), commit the metadata row and
//! its `ProcessMedia` job ([`commit_row`]), then finish inline unless `delay`
//! defers ([`finish_inline`]).
//!
//! Each helper mirrors its inline block in `stage_and_finish_upload`
//! (`src/application/media.rs` lines 356-429) verbatim in behavior, with the
//! same scalar parameter types the inline code took (not `UploadInput`; the
//! parent composes). The [`StagingGuard`] is threaded from [`stage_bytes`]
//! into [`commit_row`] so the transaction boundary is unchanged: exactly one
//! guard spans staging and the row commit, and a failed row commit still
//! rolls back everything staged.

use super::media::{FILE_TYPE_ERROR, THUMBNAIL_ERROR};
use super::media_input::UploadFile;
use crate::application::ports::{BlobStore, NewMediaUpload, Store};
use crate::domain::{Error, FocusPoint, Id, MediaAttachment, MediaState, Result};
use crate::media::ingest::Sniffed;
use crate::media::staging::StagingGuard;
use crate::media::transcode::TranscodeConfig;

/// Bytes staged for one attachment upload, awaiting the metadata commit.
///
/// Owns the [`StagingGuard`] created by [`stage_bytes`]: [`commit_row`]
/// consumes this value and commits the guard only after the metadata row (and
/// its `ProcessMedia` job) commit. Dropping it uncommitted removes everything
/// staged, exactly as the inline scope's guard did on early return.
pub struct StagedUpload<'blobs> {
    /// Guard recording every staged key; committed by [`commit_row`].
    pub staging: StagingGuard<'blobs>,
    /// Fresh `{aa}/{bb}/{random16}` shard scoping this attachment's keys.
    pub shard: String,
    /// Blob key of the staged original (`{shard}/original.{ext}`).
    pub file_key: String,
    /// Blob key of the staged operator cover image, if one was supplied.
    pub thumbnail_key: Option<String>,
}

/// Stages the original bytes (and the optional cover image) under a fresh shard.
///
/// Mirrors `stage_and_finish_upload` lines 356-379 verbatim: the guard is
/// created before any write, the original is published by rename for spooled
/// uploads (leaving a spool-file cleanup on stores without `publish_tmp`),
/// and the thumbnail is staged under the same shard. Nothing is committed.
pub fn stage_bytes<'blobs>(
    blobs: &'blobs dyn BlobStore,
    file: &mut UploadFile<'_>,
    sniffed: Sniffed,
    thumbnail: Option<(&[u8], Sniffed)>,
) -> Result<StagedUpload<'blobs>> {
    let mut staging = StagingGuard::new(blobs);
    let shard = new_shard();
    let file_key = shard_key(&shard, "original", sniffed.extension);
    match file {
        UploadFile::Bytes(bytes) => staging.put(&file_key, bytes)?,
        UploadFile::Spooled(spool) => {
            let tmp = spool.disarm();
            if let Err(err) = staging.publish_file(&tmp, &file_key) {
                // Stores without `publish_tmp` leave the spool file behind.
                let _ = std::fs::remove_file(&tmp);
                return Err(err);
            }
        }
    }
    let thumbnail_key = match &thumbnail {
        Some((bytes, thumb)) => {
            let key = shard_key(&shard, "thumbnail", thumb.extension);
            staging.put(&key, bytes)?;
            Some(key)
        }
        None => None,
    };
    Ok(StagedUpload {
        staging,
        shard,
        file_key,
        thumbnail_key,
    })
}

/// Commits the attachment metadata row (and its `ProcessMedia` job) in one transaction.
///
/// Mirrors `stage_and_finish_upload` lines 381-402 verbatim: a single
/// `create_media_upload` call carrying the staged keys, then the guard commit.
/// A failed row commit propagates with the guard still uncommitted, so every
/// staged key is rolled back exactly as inline.
// WHY: mirrors create_media_upload field-for-field in one transaction;
// UploadInput owns the upload path, this row-commit keeps the column order.
#[allow(clippy::too_many_arguments)]
pub fn commit_row(
    store: &dyn Store,
    staged: StagedUpload<'_>,
    now_ms: u64,
    account_id: Id,
    sniffed: Sniffed,
    description: &str,
    file_len: u64,
    focus: Option<FocusPoint>,
    delay: bool,
    license: Option<u8>,
) -> Result<MediaAttachment> {
    let StagedUpload {
        staging,
        file_key,
        thumbnail_key,
        ..
    } = staged;
    // ONE transaction: metadata row + owner index + ProcessMedia job.
    let media = store.create_media_upload(
        NewMediaUpload {
            account_id,
            r#type: sniffed.r#type.to_string(),
            mime_type: sniffed.mime_type.to_string(),
            description: description.to_string(),
            file_size: file_len,
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
    staging.commit();
    Ok(media)
}

/// Finishes a committed upload inline, unless `delay` defers a larger format.
///
/// Mirrors `stage_and_finish_upload` lines 404-429 verbatim: `delay` with a
/// larger media format (or a pending transcode) returns the `Pending` row for
/// the `ProcessMedia` job; otherwise the job runs now, the row is reloaded,
/// and a `Failed` row maps to Mastodon's two `rescue` bodies (`500`
/// `THUMBNAIL_ERROR` for processing failures, `422` `FILE_TYPE_ERROR` for
/// unidentifiable bytes).
pub fn finish_inline(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media: MediaAttachment,
    delay: bool,
    now_ms: u64,
    config: Option<&TranscodeConfig>,
) -> Result<MediaAttachment> {
    // Inline processing (v1 always; v2 only when the format needs no
    // deferred work). The committed job stays queued and is a no-op when a
    // worker replays it — processing is idempotent.
    let transcode_deferred = config.is_some_and(|c| c.enabled)
        && crate::media::transcode::needs_transcode(&media.mime_type);
    if delay && (media.larger_media_format() || transcode_deferred) {
        return Ok(media);
    }
    crate::application::media::process_media_job_with_config(
        store, blobs, media.id, now_ms, config,
    )?;
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

/// A fresh `{aa}/{bb}/{random16}` shard for one attachment.
///
/// Verbatim copy of the private helper in `super::media` (kept local so this
/// module compiles standalone); the parent unifies them when it wires the
/// split and removes the duplicate.
fn new_shard() -> String {
    let hex = crate::application::crypto::random_token_hex(8);
    format!("{}/{}/{}", &hex[0..2], &hex[2..4], hex)
}

/// `{shard}/{style}.{ext}`.
///
/// Verbatim copy of the private helper in `super::media`; see [`new_shard`].
fn shard_key(shard: &str, style: &str, ext: &str) -> String {
    format!("{shard}/{style}.{ext}")
}
