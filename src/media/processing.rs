//! Real media processing (Phase 4) plus the Phase 7.1 transcode decision
//! points: decoding, dimensions, blurhash and the `small` preview.
//!
//! The pure-Rust stage in this module is kept for two roles:
//!
//! - **passthrough rows** (WebP stills, audio) finish entirely here, as
//!   before;
//! - **transcoded stills** are decoded here *after* ffmpeg runs, so the
//!   committed `width`/`height`, blurhash and `small` preview always describe
//!   the derived WebP bytes, never the original.
//!
//! The transcode decision table, ffmpeg invocation, startup validation and
//! the post-commit job live in [`crate::media::transcode`]:
//!
//! - GIF (animated or not) → WebM/VP9, wire type `video` (`gifv` never);
//! - video → WebM/VP9 + Opus, except VP9/WebM + Opus-or-silent within the
//!   upload size limit, which passes through;
//! - stills → WebP (WebP passes through); the `small` preview is WebP too.
//!   HEIC/HEIF/TIFF stills (D4) always take the ffmpeg `ImageToWebp` path
//!   when transcoding is enabled; the pure-Rust decoders below cannot read
//!   them, so with transcoding disabled (or an undecodable input) they fail
//!   as [`ProcessError::Unidentifiable`] — a refused `Failed` row, never a
//!   silent success and never fabricated bytes.
//!
//! Every ffmpeg command carries `-map_metadata 0` and no strip filter:
//! metadata (EXIF, orientation tag) rides along, never applied to pixels.
//!
//! Failures are classified so the HTTP layer can reproduce Mastodon's two
//! pinned bodies: [`ProcessError::Unidentifiable`] becomes
//! `422 File type of uploaded media could not be verified` and
//! [`ProcessError::Other`] becomes
//! `500 Error processing thumbnail for uploaded media`.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};

use crate::media::upload_gate::media_work_gate;

/// Bounding box of the generated `small` preview (Mastodon: "resize to fit
/// 460x460").
const PREVIEW_BOUND: u32 = 460;

/// Largest number of concurrent blocking media jobs on the shared pool: the
/// same gate the HTTP upload path uses
/// ([`crate::media::upload_gate::MediaWorkGate`]), so a burst of remote
/// fetches enqueueing `ProcessMedia` rows shares one global bound with local
/// uploads instead of each path bursting the blocking pool on its own.
pub const MAX_MEDIA_BLOCKING_JOBS: usize = crate::media::upload_gate::DEFAULT_MEDIA_WORK_LIMIT;

/// Run CPU-bound media work (the decode/resize/encode in [`process`]) on the
/// shared bounded blocking pool.
///
/// Image decode, Lanczos resize and BlurHash are blocking work: running them
/// on the async executor would stall every other task, and spawning them
/// unbounded would let a burst of remote fetches starve Tokio. The gate
/// queues async callers (no thread is held while waiting) and runs the
/// closure on the blocking pool, at most [`MAX_MEDIA_BLOCKING_JOBS`] at a
/// time — the same bound the HTTP upload path takes, so remote influx and
/// local uploads cannot each saturate the pool independently.
pub async fn run_bounded_blocking<T, F>(work: F) -> crate::domain::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    media_work_gate().run(work).await
}

/// A processing failure, classified the way Mastodon's upload rescues are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    /// The bytes could not be identified or decoded (Paperclip's
    /// `NotIdentifiedByImageMagickError` equivalent → 422).
    Unidentifiable(String),
    /// A downstream generation step failed (Paperclip::Error equivalent →
    /// 500).
    Other(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::Unidentifiable(detail) => {
                write!(f, "unidentifiable media: {detail}")
            }
            ProcessError::Other(detail) => write!(f, "media processing failed: {detail}"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Generated `small` preview bytes (images only).
#[derive(Debug, Clone)]
pub struct Preview {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Encoded content type (`image/jpeg`, `image/png`, `image/webp`).
    pub mime_type: &'static str,
    /// Canonical extension of the encoded bytes.
    pub extension: &'static str,
}

/// Outcome of processing an attachment's original bytes.
///
/// `meta.small` is only rendered by the DTO when `preview` is `Some` (i.e.
/// only when a preview was actually generated).
#[derive(Debug, Default, Clone)]
pub struct Processed {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub blurhash: Option<String>,
    pub preview: Option<Preview>,
    /// Container duration in milliseconds (reserved; no audio/video metadata
    /// parsing ships without a transcoding toolchain).
    pub duration_ms: Option<u64>,
}

/// Process the original bytes of an attachment classified as `mime_type`
/// (the *sniffed* content type, never the declared one).
///
/// HEIC/HEIF/TIFF have no pure-Rust decoder here on purpose: those rows take
/// the ffmpeg `ImageToWebp` job when transcoding is enabled, and answering
/// `Unidentifiable` on the legacy path keeps a toolchain-less server honest
/// (the row commits `Failed`, surfaced as 422) instead of guessing.
pub fn process(mime_type: &str, bytes: &[u8]) -> Result<Processed, ProcessError> {
    if mime_type.starts_with("image/") {
        return process_image(mime_type, bytes);
    }
    match mime_type {
        "video/mp4" | "audio/mp4" => {
            let (width, height) = mp4_dimensions(bytes);
            Ok(Processed {
                width,
                height,
                ..Processed::default()
            })
        }
        // Accepted containers with no metadata parser: dimensions stay
        // `None` (honest "unknown", never fabricated).
        "video/webm" | "audio/ogg" | "audio/mpeg" | "audio/wav" | "audio/flac" => {
            Ok(Processed::default())
        }
        other => Err(ProcessError::Unidentifiable(format!(
            "unsupported media type {other}"
        ))),
    }
}

/// Decode an image, hash it and generate the `small` preview.
fn process_image(mime_type: &str, bytes: &[u8]) -> Result<Processed, ProcessError> {
    let format = image_format(mime_type).ok_or_else(|| {
        ProcessError::Unidentifiable(format!("unsupported media type {mime_type}"))
    })?;
    check_decode_limits(format, bytes)?;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(decode_limits());
    let img = reader.decode().map_err(|err| {
        ProcessError::Unidentifiable(format!("could not decode {mime_type}: {err}"))
    })?;
    let (width, height) = img.dimensions();

    // The blurhash and the preview both work off the (≤460px) preview image:
    // visually identical to hashing the original, and bounded work per upload.
    let (preview_w, preview_h) = fit_within(width, height, PREVIEW_BOUND);
    let preview_img = if (preview_w, preview_h) == (width, height) {
        img
    } else {
        img.resize(preview_w, preview_h, FilterType::Lanczos3)
    };

    let rgba = preview_img.to_rgba8();
    let (components_x, components_y) = blurhash_components(preview_w, preview_h);
    // Encode-only use: this is the single `blurhash::` call in the tree, and
    // `decode` is never called (nor reachable through any dependency), so the
    // crafted-input decode advisory (RUSTSEC-2023-0083) does not apply here.
    let blurhash = blurhash::encode(
        components_x,
        components_y,
        preview_w,
        preview_h,
        rgba.as_raw(),
    );

    // Re-encode in the source format where one exists; GIF/BMP previews are
    // PNG (animated GIFs would lose animation either way).
    let (preview_format, preview_mime, preview_extension) = match format {
        ImageFormat::Jpeg => (ImageFormat::Jpeg, "image/jpeg", "jpg"),
        ImageFormat::Png => (ImageFormat::Png, "image/png", "png"),
        ImageFormat::WebP => (ImageFormat::WebP, "image/webp", "webp"),
        _ => (ImageFormat::Png, "image/png", "png"),
    };
    let mut cursor = Cursor::new(Vec::new());
    preview_img
        .write_to(&mut cursor, preview_format)
        .map_err(|err| ProcessError::Other(format!("encode preview: {err}")))?;

    Ok(Processed {
        width: Some(width),
        height: Some(height),
        blurhash: Some(blurhash),
        preview: Some(Preview {
            bytes: cursor.into_inner(),
            width: preview_w,
            height: preview_h,
            mime_type: preview_mime,
            extension: preview_extension,
        }),
        duration_ms: None,
    })
}

/// Most pixels an accepted still image may have (Mastodon's
/// `MAX_IMAGE_PIXELS`, about an 8K frame). A compressed byte cap alone does
/// not bound the decoded size: a small PNG can declare billions of pixels.
pub const MAX_IMAGE_PIXELS: u64 = 33_177_600;

/// Longest accepted image side. Bounds degenerate `1 x 100_000_000` frames
/// that pass the pixel product on a 32-bit multiplication.
pub const MAX_IMAGE_SIDE: u32 = 20_000;

/// Decoder allocation ceiling: one RGBA frame at the pixel cap (4 bytes per
/// pixel) plus headroom for the decoder's working buffers.
const MAX_DECODE_ALLOC_BYTES: u64 = MAX_IMAGE_PIXELS * 4 + 64 * 1024 * 1024;

/// Decoder limits applied to every untrusted decode.
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

/// Read the declared dimensions from the container header only (no pixel
/// decode) and refuse oversized images before any allocation.
fn check_decode_limits(format: ImageFormat, bytes: &[u8]) -> Result<(), ProcessError> {
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader.into_dimensions().map_err(|err| {
        ProcessError::Unidentifiable(format!("could not read image header: {err}"))
    })?;
    let pixels: u64 = u64::from(width) * u64::from(height);
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE || pixels > MAX_IMAGE_PIXELS {
        return Err(ProcessError::Unidentifiable(format!(
            "image dimensions {width}x{height} exceed the limit"
        )));
    }
    Ok(())
}

/// Header-only dimension check for a sniffed still image, shared with the
/// transcode path so ffmpeg is never started on a decompression bomb.
/// Formats the pure-Rust decoders cannot read (HEIC, TIFF) pass through.
pub fn check_image_limits(mime_type: &str, bytes: &[u8]) -> Result<(), ProcessError> {
    match image_format(mime_type) {
        Some(format) => check_decode_limits(format, bytes),
        None => Ok(()),
    }
}

/// Map a sniffed content type onto an `image`-crate format.
fn image_format(mime_type: &str) -> Option<ImageFormat> {
    match mime_type {
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        "image/bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

/// Scale `(width, height)` to fit inside a `bound`x`bound` box (never
/// upscaling, never 0x0).
fn fit_within(width: u32, height: u32, bound: u32) -> (u32, u32) {
    let scale = (f64::from(bound) / f64::from(width))
        .min(f64::from(bound) / f64::from(height))
        .min(1.0);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// Blurhash component counts: 4x3 for landscape-ish previews (Mastodon's
/// constant), 3x4 for portrait, 4x4 for small square-ish images.
fn blurhash_components(width: u32, height: u32) -> (u32, u32) {
    if width <= 64 && height <= 64 {
        (4, 4)
    } else if width >= height {
        (4, 3)
    } else {
        (3, 4)
    }
}

/// Read MP4/QuickTime dimensions from the first `tkhd` box carrying them.
///
/// The `tkhd` payload always ends with `width`/`height` as 16.16 fixed-point
/// values (version 0 and 1 only differ in the prefix), so the box tail is
/// enough — no full ISO-BMFF model is built.
pub fn mp4_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    find_tkhd_size(bytes).unwrap_or((None, None))
}

/// Walk the ISO-BMFF box tree for a `tkhd` box with a positive size.
fn find_tkhd_size(bytes: &[u8]) -> Option<(Option<u32>, Option<u32>)> {
    // Container boxes a `tkhd` can live in (`moov/trak/...`).
    const CONTAINERS: [&[u8; 4]; 7] = [
        b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta",
    ];
    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as u64;
        let kind: &[u8; 4] = bytes[offset + 4..offset + 8].try_into().ok()?;
        let (start, end) = if size == 1 {
            // 64-bit largesize.
            if offset + 16 > bytes.len() {
                return None;
            }
            let large = u64::from_be_bytes(bytes[offset + 8..offset + 16].try_into().ok()?);
            if large < 16 {
                return None;
            }
            (offset + 16, offset + large as usize)
        } else if size == 0 {
            // Box extends to the end of the file.
            (offset + 8, bytes.len())
        } else {
            if size < 8 {
                return None;
            }
            (offset + 8, offset + size as usize)
        };
        if start > bytes.len() || start > end {
            return None;
        }
        let end = end.min(bytes.len());
        let content = &bytes[start..end];
        if kind == b"tkhd" {
            let (width, height) = tkhd_size(content);
            // Skip audio-only tracks (0x0) and keep looking.
            if width.is_some() && height.is_some() {
                return Some((width, height));
            }
        } else if CONTAINERS.contains(&kind)
            && let Some(found) = find_tkhd_size(content)
            && found.0.is_some()
        {
            return Some(found);
        }
        if size == 0 {
            break;
        }
        offset = end;
    }
    None
}

/// The trailing 16.16 fixed-point width/height of a `tkhd` payload.
fn tkhd_size(content: &[u8]) -> (Option<u32>, Option<u32>) {
    if content.len() < 8 {
        return (None, None);
    }
    let tail = &content[content.len() - 8..];
    let width = u32::from_be_bytes(tail[0..4].try_into().ok().unwrap()) >> 16;
    let height = u32::from_be_bytes(tail[4..8].try_into().ok().unwrap()) >> 16;
    ((width > 0).then_some(width), (height > 0).then_some(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(img: &image::DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, format).expect("encode fixture");
        cursor.into_inner()
    }

    fn fixture(format: ImageFormat) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(24, 12, |x, y| {
            image::Rgba([(x * 10) as u8, (y * 20) as u8, 128, 255])
        }));
        let img = if format == ImageFormat::Jpeg {
            image::DynamicImage::ImageRgb8(img.to_rgb8())
        } else {
            img
        };
        encode(&img, format)
    }

    #[test]
    fn png_round_trip_reports_dims_blurhash_and_preview() {
        let bytes = fixture(ImageFormat::Png);
        let processed = process("image/png", &bytes).expect("process png");
        assert_eq!(processed.width, Some(24));
        assert_eq!(processed.height, Some(12));
        let blurhash = processed.blurhash.expect("blurhash");
        assert!(!blurhash.is_empty());
        let preview = processed.preview.expect("preview");
        assert_eq!(preview.mime_type, "image/png");
        assert_eq!(preview.extension, "png");
        assert!(preview.width <= PREVIEW_BOUND && preview.height <= PREVIEW_BOUND);
        // The preview itself decodes as a PNG of the reported size.
        let decoded = image::load_from_memory(&preview.bytes).expect("decode preview");
        assert_eq!(decoded.dimensions(), (preview.width, preview.height));
    }

    #[test]
    fn jpeg_and_webp_keep_their_format_in_the_preview() {
        let jpeg = process("image/jpeg", &fixture(ImageFormat::Jpeg)).expect("jpeg");
        assert_eq!(jpeg.preview.as_ref().unwrap().mime_type, "image/jpeg");
        let webp = process("image/webp", &fixture(ImageFormat::WebP)).expect("webp");
        assert_eq!(webp.preview.as_ref().unwrap().mime_type, "image/webp");
        // Small images are not upscaled.
        assert_eq!((jpeg.width, jpeg.height), (Some(24), Some(12)));
    }

    #[test]
    fn gif_and_bmp_previews_are_png() {
        let gif = process("image/gif", &fixture(ImageFormat::Gif)).expect("gif");
        assert_eq!(gif.preview.as_ref().unwrap().mime_type, "image/png");
        let bmp = process("image/bmp", &fixture(ImageFormat::Bmp)).expect("bmp");
        assert_eq!(bmp.preview.as_ref().unwrap().mime_type, "image/png");
    }

    #[test]
    fn large_images_scale_down_to_the_preview_bound() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(1200, 600));
        let bytes = encode(&img, ImageFormat::Png);
        let processed = process("image/png", &bytes).expect("process");
        let preview = processed.preview.expect("preview");
        assert_eq!((preview.width, preview.height), (460, 230));
    }

    #[test]
    fn corrupt_or_unsupported_bytes_are_unidentifiable() {
        // Right magic bytes, truncated payload: decoding fails.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0; 16]);
        assert!(matches!(
            process("image/png", &png),
            Err(ProcessError::Unidentifiable(_))
        ));
        assert!(matches!(
            process("application/pdf", &[0; 32]),
            Err(ProcessError::Unidentifiable(_))
        ));
    }

    #[test]
    fn mp4_dimensions_from_tkhd_box() {
        // moov > trak > tkhd; payload ends with 16.16 width/height.
        let mut tkhd = vec![0u8; 84];
        let tail = [(640u32 << 16).to_be_bytes(), (480u32 << 16).to_be_bytes()].concat();
        tkhd[76..84].copy_from_slice(&tail);
        let mut trak = Vec::new();
        trak.extend_from_slice(&((tkhd.len() + 8) as u32).to_be_bytes());
        trak.extend_from_slice(b"tkhd");
        trak.extend_from_slice(&tkhd);
        let mut moov = Vec::new();
        moov.extend_from_slice(&((trak.len() + 8) as u32).to_be_bytes());
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&trak);
        let mut mp4 = vec![0u8, 0, 0, 0x14];
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0, 0, 0, 0]);
        mp4.extend_from_slice(b"isom");
        mp4.extend_from_slice(&moov);

        assert_eq!(mp4_dimensions(&mp4), (Some(640), Some(480)));

        // No moov at all: dimensions stay None, never fabricated.
        let mut bare = vec![0u8, 0, 0, 0x18];
        bare.extend_from_slice(b"ftypisom");
        bare.extend_from_slice(&[0; 16]);
        assert_eq!(mp4_dimensions(&bare), (None, None));
    }

    #[test]
    fn audio_and_webm_are_accepted_without_dimensions() {
        let ogg = process("audio/ogg", b"OggS some audio bytes here").expect("ogg");
        assert_eq!((ogg.width, ogg.height), (None, None));
        assert!(ogg.preview.is_none());
        let webm = process(
            "video/webm",
            &[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .expect("webm");
        assert_eq!((webm.width, webm.height), (None, None));
    }

    #[test]
    fn fit_within_never_upscales_or_produces_zero() {
        assert_eq!(fit_within(100, 50, 460), (100, 50));
        assert_eq!(fit_within(920, 460, 460), (460, 230));
        assert_eq!(fit_within(1, 1000, 460), (1, 460));
    }
}
