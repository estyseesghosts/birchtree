//! Derived-blob metadata and failure commit (S12 split of `super`).

use crate::application::media::UNSUPPORTED_REASON;
use crate::application::ports::{MediaProcessingResult, Store};
use crate::domain::{MediaState, Result};
use crate::media::processing;

use super::commit::TranscodeCtx;
use super::config::TranscodeConfig;
use super::plan::TranscodePlan;
use super::probe::probe_file;
use super::temp::{TempGuard, temp_path};

/// Meta + (for stills) preview/blurhash from the DERIVED bytes, never the
/// original: dimensions must match the stored pixels (`commit_derived`
/// stage 1, verbatim). Returns `None` when the meta read already committed
/// `Failed` (undecodable still) or the plan needs no meta (`Passthrough`).
pub(super) fn derived_meta(
    ctx: &TranscodeCtx,
    plan: TranscodePlan,
    media: &crate::domain::MediaAttachment,
    derived: &[u8],
) -> Result<
    Option<(
        Option<u32>,
        Option<u32>,
        Option<String>,
        Option<crate::media::processing::Preview>,
        Option<u64>,
    )>,
> {
    let store = ctx.store;
    let now_ms = ctx.now_ms;
    let config = ctx.config;
    Ok(match plan {
        TranscodePlan::ImageToWebp => match processing::process("image/webp", derived) {
            Ok(processed) => Some((
                processed.width,
                processed.height,
                processed.blurhash,
                processed.preview,
                None,
            )),
            Err(err) => {
                tracing::warn!(media = media.id.value(), error = %err, "transcoded image undecodable");
                mark_failed(
                    store,
                    media,
                    &format!("{}: ffmpeg output undecodable: {err}", UNSUPPORTED_REASON),
                    now_ms,
                )?;
                None
            }
        },
        TranscodePlan::GifToWebm | TranscodePlan::VideoToWebm | TranscodePlan::VideoRemux => {
            let (width, height, duration_ms) = probe_derived(derived, config);
            Some((width, height, None, None, duration_ms))
        }
        TranscodePlan::Passthrough => None,
    })
}

/// Probe derived video bytes for commit meta (dims + duration when known).
fn probe_derived(
    derived: &[u8],
    config: &TranscodeConfig,
) -> (Option<u32>, Option<u32>, Option<u64>) {
    let output_path = temp_path("derived", "webm");
    let _guard = TempGuard::new(vec![output_path.clone()]);
    if std::fs::write(&output_path, derived).is_err() {
        return (None, None, None);
    }
    match probe_file(&config.ffmpeg_path, &output_path) {
        Ok(probe) => (probe.width, probe.height, probe.duration_ms),
        Err(_) => (None, None, None),
    }
}

/// Commit a `Failed` state (job still completes; the API answers `422`).
pub(super) fn mark_failed(
    store: &dyn Store,
    media: &crate::domain::MediaAttachment,
    reason: &str,
    now_ms: u64,
) -> Result<()> {
    // Best-effort: a crashed attempt's staging must never be served or
    // accumulated; the retry overwrites the deterministic key anyway.
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

/// A sibling key in the same shard as `file_key` (`{dir}/{style}.{ext}`).
pub(super) fn sibling_key(file_key: &str, style: &str, ext: &str) -> Option<String> {
    crate::media::public_media::sibling_key(file_key, style, ext)
}
