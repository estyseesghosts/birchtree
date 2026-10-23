//! Durable transcode job payload and handler (S12 split of `super`).

use crate::application::media::{THUMBNAIL_ERROR, UNSUPPORTED_REASON};
use crate::application::ports::{BlobStore, MediaProcessingResult, Store};
use crate::domain::{Error, Id, MediaState, Result};
use crate::media::processing;
use crate::media::staging::StagingGuard;

use super::argv::build_argv;
use super::commit::{TranscodeCtx, run_transcode_plan};
use super::commit_meta::{mark_failed, sibling_key};
use super::config::TranscodeConfig;
use super::plan::{TranscodePlan, derived_extension, plan_for_mime};
use super::run::{RunStats, TranscodeError, run_bounded};
use super::temp::{TempGuard, temp_path};
use super::toolchain::validate_toolchain;

/// Transcode `input_bytes` (extension `input_ext`, a hint for ffmpeg's
/// demuxer only) into the plan's derived bytes.
pub fn transcode_bytes(
    input_bytes: &[u8],
    input_ext: &str,
    plan: TranscodePlan,
    with_audio: bool,
    config: &TranscodeConfig,
) -> std::result::Result<(Vec<u8>, RunStats), TranscodeError> {
    let input_path = temp_path("in", input_ext);
    let output_path = temp_path("out", derived_extension(plan));
    let _guard = TempGuard::new(vec![input_path.clone(), output_path.clone()]);
    std::fs::write(&input_path, input_bytes)
        .map_err(|err| TranscodeError::Io(format!("write transcode input: {err}")))?;
    let argv = build_argv(&input_path, &output_path, plan, with_audio, config);
    let stats = run_bounded(&argv, Some(&output_path), &config.limits())?;
    let derived = std::fs::read(&output_path)
        .map_err(|err| TranscodeError::Io(format!("read transcode output: {err}")))?;
    Ok((derived, stats))
}

// ---------------------------------------------------------------------------
// Durable job payload and handler.
// ---------------------------------------------------------------------------

/// Payload of the post-commit transcode job, enqueued under
/// `JobKind::TranscodeMedia` and dispatched by the job worker (ffmpeg runs
/// off the async runtime); the sync drain funnels the same payload through
/// [`run_transcode_job`] when a toolchain is threaded, and completes it
/// without work otherwise.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscodeJobPayload {
    pub media_id: Id,
}

impl TranscodeJobPayload {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "media_id": self.media_id.value() })
    }

    /// Extract the media id from a job payload (`Err` when absent: a
    /// programmer error, surfaced loudly rather than silently skipped).
    pub fn media_id_from_json(payload: &serde_json::Value) -> Result<Id> {
        payload
            .get("media_id")
            .and_then(|v| v.as_u64())
            .map(Id::new)
            .ok_or_else(|| Error::Internal("transcode job: missing media_id".into()))
    }
}

/// Execute a transcode job: finish processing staged bytes and commit the
/// result (idempotent).
///
/// Contract: a missing row, or a row already `Ready`/`Failed`, is a no-op
/// `Ok(())`. Rows that need no transcode take the legacy pure-Rust path.
/// Transcoded rows run ffmpeg outside any transaction, stage the derived
/// blob, overwrite the attachment's own `file_key` (derived *replaces*
/// original — no pointer swap, no orphan accumulation), and commit `Ready`
/// last with meta read from the *derived* bytes. Deterministic failures
/// commit `Failed` (the API answers `422`); only missing/unreadable storage
/// (and a vanishing toolchain) return `Err` so the job retries under its
/// bounded budget and then quarantines.
pub fn run_transcode_job(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media_id: Id,
    now_ms: u64,
    config: &TranscodeConfig,
) -> Result<()> {
    let Some(media) = store.media_by_id(media_id)? else {
        return Ok(());
    };
    if matches!(media.state, MediaState::Ready | MediaState::Failed) {
        return Ok(());
    }
    if media.file_key.is_empty() {
        mark_failed(
            store,
            &media,
            &format!("{}: no staged bytes", UNSUPPORTED_REASON),
            now_ms,
        )?;
        return Ok(());
    }
    let Some(bytes) = blobs.get(&media.file_key)? else {
        return Err(Error::Storage(format!(
            "media {media_id}: staged original missing"
        )));
    };
    match plan_for_mime(&media.mime_type) {
        TranscodePlan::Passthrough => commit_legacy(store, blobs, &media, &bytes, now_ms),
        plan => {
            if !config.enabled {
                return Err(Error::Internal(
                    "transcode job ran while transcoding is disabled".into(),
                ));
            }
            if let Err(detail) = validate_toolchain(&config.ffmpeg_path) {
                // Startup validation should have caught this; a binary that
                // vanishes at runtime is environmental → retry, then
                // quarantine. Never fall back to serving the original.
                return Err(Error::Storage(format!(
                    "ffmpeg toolchain unavailable: {detail}"
                )));
            }
            run_transcode_plan(
                &TranscodeCtx {
                    store,
                    blobs,
                    now_ms,
                    config,
                },
                &media,
                &bytes,
                plan,
            )
        }
    }
}

/// Legacy pure-Rust finish for rows that need no transcode (WebP, audio):
/// decode for dims/blurhash/small preview and commit, mirroring the
/// pre-7.1 `process_media_job` outcome byte-for-byte.
fn commit_legacy(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media: &crate::domain::MediaAttachment,
    bytes: &[u8],
    now_ms: u64,
) -> Result<()> {
    match processing::process(&media.mime_type, bytes) {
        Ok(processed) => {
            let mut staging = StagingGuard::new(blobs);
            let mut preview_key = None;
            let mut preview_width = None;
            let mut preview_height = None;
            if let Some(preview) = processed.preview {
                let key = sibling_key(&media.file_key, "small", preview.extension)
                    .ok_or_else(|| Error::Internal("media key carries no shard".into()))?;
                staging.put(&key, &preview.bytes)?;
                preview_key = Some(key);
                preview_width = Some(preview.width);
                preview_height = Some(preview.height);
            }
            store.mark_media_processed(
                media.id,
                MediaProcessingResult {
                    state: MediaState::Ready,
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
            staging.commit();
        }
        Err(err) => {
            tracing::warn!(media = media.id.value(), error = %err, "media processing failed");
            let reason = match &err {
                processing::ProcessError::Unidentifiable(detail) => {
                    format!("{}: {detail}", UNSUPPORTED_REASON)
                }
                processing::ProcessError::Other(_) => THUMBNAIL_ERROR.to_string(),
            };
            mark_failed(store, media, &reason, now_ms)?;
        }
    }
    Ok(())
}
