//! Job-time plan execution and commit (S12 split of `super`).

use crate::application::media::UNSUPPORTED_REASON;
use crate::application::ports::{BlobStore, MediaProcessingResult, Store};
use crate::domain::{Error, MediaState, Result};
use crate::media::processing;
use crate::media::staging::StagingGuard;

use super::commit_meta::{derived_meta, mark_failed, sibling_key};
use super::config::TranscodeConfig;
use super::job::transcode_bytes;
use super::plan::{TranscodePlan, derived_extension};
use super::probe::{Probe, probe_file, probe_is_passthrough};
use super::run::TranscodeError;
use super::temp::{TempGuard, staging_key_for, temp_path};

/// Run the ffmpeg plan for `media` and commit the derived result.
/// Execution context for the transcode job path: the two stores, the job
/// timestamp and the operator's transcode knobs. Threaded through
/// `run_transcode_plan` / `commit_derived` so neither takes 7–8 params.
/// `config` carries ffmpeg path, caps and quality knobs; no value is copied
/// out. No behavior change: field-for-field the old parameter lists.
pub struct TranscodeCtx<'a> {
    /// Content store (`mark_media_processed`, `media_by_id`).
    pub store: &'a dyn Store,
    /// Blob store (stage derived, overwrite original, verify read-back).
    pub blobs: &'a dyn BlobStore,
    /// Job timestamp for the `Ready`/`Failed` row commit.
    pub now_ms: u64,
    /// Operator transcode knobs (ffmpeg path, caps, quality).
    pub config: &'a TranscodeConfig,
}

pub(super) fn run_transcode_plan(
    ctx: &TranscodeCtx,
    media: &crate::domain::MediaAttachment,
    bytes: &[u8],
    plan: TranscodePlan,
) -> Result<()> {
    let store = ctx.store;
    let blobs = ctx.blobs;
    let now_ms = ctx.now_ms;
    let config = ctx.config;
    // A compressed byte cap does not bound decoded size: refuse oversized
    // stills from their header before FFmpeg allocates anything.
    if plan == TranscodePlan::ImageToWebp
        && let Err(err) = processing::check_image_limits(&media.mime_type, bytes)
    {
        mark_failed(
            store,
            media,
            &format!("{}: {err}", UNSUPPORTED_REASON),
            now_ms,
        )?;
        return Ok(());
    }
    // Video probes the original first: VP9 + Opus-or-silent within the size
    // limit keeps its codecs (D3) — byte-identical when already WebM,
    // stream-copied into WebM otherwise.
    if plan == TranscodePlan::VideoToWebm
        && let Some(probe) = probe_video(bytes, config)
    {
        if probe.container_is_webm() {
            return commit_passthrough_video(store, media, bytes, now_ms, config);
        }
        return run_remux_to_webm(store, blobs, media, bytes, now_ms, config);
    }
    let with_audio = plan == TranscodePlan::VideoToWebm && probe_has_audio(bytes, config);
    // Demuxer hint only (ffmpeg sniffs content): real extensions keep ffmpeg
    // from mis-probing BOM-less stills, and bound the temp-file namespace.
    let input_ext = match media.mime_type.as_str() {
        "image/heic" | "image/heif" => "heic",
        "image/tiff" | "image/x-tiff" => "tif",
        _ => match plan {
            TranscodePlan::ImageToWebp => "img",
            TranscodePlan::GifToWebm => "gif",
            TranscodePlan::VideoToWebm | TranscodePlan::VideoRemux => "vid",
            TranscodePlan::Passthrough => "bin",
        },
    };
    match transcode_bytes(bytes, input_ext, plan, with_audio, config) {
        Ok((derived, _stats)) => commit_derived(ctx, media, bytes, &derived, plan),
        Err(err) => match err {
            TranscodeError::Spawn(detail) | TranscodeError::Io(detail) => {
                Err(Error::Storage(format!(
                    "transcode of media {} failed transiently: {detail}",
                    media.id.value()
                )))
            }
            other => {
                tracing::warn!(media = media.id.value(), error = %other, "media transcode failed");
                mark_failed(store, media, &format!("transcode failed: {other}"), now_ms)?;
                Ok(())
            }
        },
    }
}

/// Probe `bytes` for the D3 codec bound. Returns the probe when the codecs
/// are already VP9 + Opus-or-silent within the size limit (`None` otherwise,
/// including any probe failure → the job transcodes). The caller splits
/// WebM (byte passthrough) from other containers (stream-copy remux).
fn probe_video(bytes: &[u8], config: &TranscodeConfig) -> Option<Probe> {
    let input_path = temp_path("probe", "vid");
    let _guard = TempGuard::new(vec![input_path.clone()]);
    std::fs::write(&input_path, bytes).ok()?;
    let probe = probe_file(&config.ffmpeg_path, &input_path).ok()?;
    probe_is_passthrough(&probe, bytes.len() as u64, config.max_file_size).then_some(probe)
}

/// Remux VP9 + Opus-or-silent bytes from a non-WebM container into WebM
/// with stream copy (no re-encode), then commit exactly like a transcode:
/// derived replaces the original, meta comes from the remuxed bytes. A
/// remux failure is deterministic input → `Failed`, never a quiet keep of
/// the non-WebM bytes.
fn run_remux_to_webm(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media: &crate::domain::MediaAttachment,
    bytes: &[u8],
    now_ms: u64,
    config: &TranscodeConfig,
) -> Result<()> {
    let ctx = TranscodeCtx {
        store,
        blobs,
        now_ms,
        config,
    };
    match transcode_bytes(bytes, "vid", TranscodePlan::VideoRemux, false, config) {
        Ok((derived, _stats)) => {
            commit_derived(&ctx, media, bytes, &derived, TranscodePlan::VideoToWebm)
        }
        Err(err) => match err {
            TranscodeError::Spawn(detail) | TranscodeError::Io(detail) => {
                Err(Error::Storage(format!(
                    "remux of media {} failed transiently: {detail}",
                    media.id.value()
                )))
            }
            other => {
                tracing::warn!(media = media.id.value(), error = %other, "media remux failed");
                mark_failed(store, media, &format!("remux failed: {other}"), now_ms)?;
                Ok(())
            }
        },
    }
}

/// Whether the original carries an audio stream (decides the `-map 0:a:0`
/// vs `-an` video command). A seen audio stream maps it; silence (or an
/// unreadable probe, conservatively treated as "maybe audio") maps the audio
/// stream too — ffmpeg then errors deterministically on a truly missing
/// stream, which commits `Failed` loudly, instead of `-an` silently dropping
/// real audio.
fn probe_has_audio(bytes: &[u8], config: &TranscodeConfig) -> bool {
    let input_path = temp_path("probe", "vid");
    let _guard = TempGuard::new(vec![input_path.clone()]);
    if std::fs::write(&input_path, bytes).is_err() {
        return true;
    }
    probe_file(&config.ffmpeg_path, &input_path)
        .map(|probe| probe.audio_codec.is_some())
        .unwrap_or(true)
}

/// Commit a passthrough video: original bytes stay, the row goes `Ready`
/// with probe dims when the probe could read them (honest `None` otherwise).
fn commit_passthrough_video(
    store: &dyn Store,
    media: &crate::domain::MediaAttachment,
    bytes: &[u8],
    now_ms: u64,
    config: &TranscodeConfig,
) -> Result<()> {
    let input_path = temp_path("probe", "webm");
    let _guard = TempGuard::new(vec![input_path.clone()]);
    let (width, height, duration_ms) = if std::fs::write(&input_path, bytes).is_ok()
        && let Ok(probe) = probe_file(&config.ffmpeg_path, &input_path)
    {
        (probe.width, probe.height, probe.duration_ms)
    } else {
        (None, None, None)
    };
    store.mark_media_processed(
        media.id,
        MediaProcessingResult {
            state: MediaState::Ready,
            r#type: media.r#type.clone(),
            blurhash: None,
            preview_key: None,
            preview_width: None,
            preview_height: None,
            width,
            height,
            duration_ms,
            frame_rate: None,
            bitrate: None,
            error: None,
        },
        now_ms,
    )?;
    Ok(())
}

/// Commit a transcoded derived blob: verify it, overwrite the attachment's
/// own `file_key` (derived replaces original), stage the still-image
/// preview, and commit `Ready` with meta read from the *derived* bytes.
fn commit_derived(
    ctx: &TranscodeCtx,
    media: &crate::domain::MediaAttachment,
    original_bytes: &[u8],
    derived: &[u8],
    plan: TranscodePlan,
) -> Result<()> {
    let store = ctx.store;
    let blobs = ctx.blobs;
    let now_ms = ctx.now_ms;
    // 1. Meta + (for stills) preview/blurhash come from the DERIVED bytes,
    //    never the original: dimensions must match the stored pixels.
    let Some((width, height, blurhash, preview, duration_ms)) =
        derived_meta(ctx, plan, media, derived)?
    else {
        return Ok(());
    };
    // 2. Stage the derived blob under its deterministic key and verify the
    //    read-back before it can ever be served.
    let staging_key = staging_key_for(&media.file_key, derived_extension(plan))
        .ok_or_else(|| Error::Internal("media key carries no shard".into()))?;
    let mut staging = StagingGuard::new(blobs);
    staging.put(&staging_key, derived)?;
    let staged = blobs.get(&staging_key)?.ok_or_else(|| {
        Error::Storage(format!(
            "media {}: staged derived missing",
            media.id.value()
        ))
    })?;
    if staged.len() != derived.len() {
        return Err(Error::Storage(format!(
            "media {}: staged derived failed read-back",
            media.id.value()
        )));
    }
    // 3. Derived replaces original at the attachment's own key; verify the
    //    write (one repair attempt from memory), else retry the job.
    blobs.put(&media.file_key, derived)?;
    if blobs.get(&media.file_key)?.as_deref() != Some(derived) {
        blobs.put(&media.file_key, derived)?;
        if blobs.get(&media.file_key)?.as_deref() != Some(derived) {
            return Err(Error::Storage(format!(
                "media {}: derived blob failed read-back",
                media.id.value()
            )));
        }
    }
    // 4. Stage the still-image preview (WebP) before the row commits.
    let mut preview_key = None;
    let mut preview_width = None;
    let mut preview_height = None;
    if let Some(preview) = preview {
        let key = sibling_key(&media.file_key, "small", preview.extension)
            .ok_or_else(|| Error::Internal("media key carries no shard".into()))?;
        staging.put(&key, &preview.bytes)?;
        preview_key = Some(key);
        preview_width = Some(preview.width);
        preview_height = Some(preview.height);
    }
    // 5. Commit `Ready` last. The wire type follows the derived blob: GIF →
    //    `video`; stills stay `image`; video stays `video`. (`gifv` never.)
    let wire_type = match plan {
        TranscodePlan::GifToWebm => "video".to_string(),
        _ => media.r#type.clone(),
    };
    store.mark_media_processed(
        media.id,
        MediaProcessingResult {
            state: MediaState::Ready,
            r#type: wire_type,
            blurhash,
            preview_key,
            preview_width,
            preview_height,
            width,
            height,
            duration_ms,
            frame_rate: None,
            bitrate: None,
            error: None,
        },
        now_ms,
    )?;
    staging.commit();
    // 6. Best-effort staging cleanup (a crash before this leaves a
    //    deterministic orphan the next run overwrites).
    if let Err(err) = blobs.delete(&staging_key) {
        tracing::warn!(media = media.id.value(), error = %err, "staged derived blob left in place");
    }
    let _ = original_bytes;
    Ok(())
}
