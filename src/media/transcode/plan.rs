//! Static transcode-plan decision (S12 split of `super`).

// ---------------------------------------------------------------------------
// Transcode decision.
// ---------------------------------------------------------------------------

/// What a stored upload needs. The video codec bound (VP9 + Opus-or-silent
/// within the size limit) is decided at job time by probing the bytes
/// (`probe_video` in `commit`); WebM keeps its bytes, other containers remux. The
/// static plan only says the target shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodePlan {
    /// Already in target shape, or a class transcoding never touches
    /// (audio, unknown): keep the legacy processing path.
    Passthrough,
    /// Still image → WebP (wire type stays `image`).
    ImageToWebp,
    /// GIF (animated or not) → WebM/VP9 (wire type becomes `video`).
    GifToWebm,
    /// Video → WebM/VP9 + Opus (wire type stays `video`). At job time the
    /// bytes are probed first: VP9 + Opus-or-silent within the size limit
    /// keeps its codecs — byte-identical when already WebM, stream-copied
    /// (`-c copy`, no re-encode) into WebM otherwise.
    VideoToWebm,
    /// Job-time refinement of [`TranscodePlan::VideoToWebm`], never returned
    /// by [`plan_for_mime`]: codecs are already VP9 + Opus-or-silent but the
    /// container is not WebM, so remux with stream copy (no re-encode, no
    /// quality loss) into WebM.
    VideoRemux,
}

/// Static plan for a sniffed content type. Probe-dependent passthrough (VP9
/// WebM) still reports [`TranscodePlan::VideoToWebm`]; the job probes the
/// bytes before deciding.
pub fn plan_for_mime(mime_type: &str) -> TranscodePlan {
    match mime_type {
        "image/gif" => TranscodePlan::GifToWebm,
        // D4 stills → WebP. `image/x-tiff` is the legacy alias for TIFF
        // (the sniffer reports canonical `image/tiff`); both spellings map
        // here so a row carrying either one still converges to WebP.
        "image/jpeg" | "image/png" | "image/bmp" | "image/heic" | "image/heif" | "image/tiff"
        | "image/x-tiff" => TranscodePlan::ImageToWebp,
        "image/webp" => TranscodePlan::Passthrough,
        mime if mime.starts_with("video/") => TranscodePlan::VideoToWebm,
        _ => TranscodePlan::Passthrough,
    }
}

/// True when the upload needs an ffmpeg transcode (as opposed to the legacy
/// pure-Rust path). Used by the v2 `202` deferral hook: `delay &&
/// (larger_media_format || needs_transcode)` — a GIF is sniffed `image` but
/// still defers.
pub fn needs_transcode(mime_type: &str) -> bool {
    !matches!(plan_for_mime(mime_type), TranscodePlan::Passthrough)
}

/// Canonical extension of a plan's derived blob.
pub fn derived_extension(plan: TranscodePlan) -> &'static str {
    match plan {
        TranscodePlan::ImageToWebp | TranscodePlan::Passthrough => "webp",
        TranscodePlan::GifToWebm | TranscodePlan::VideoToWebm | TranscodePlan::VideoRemux => "webm",
    }
}
