//! Round 3 area B: HEIC/HEIF/TIFF stills → WebP (D4).
//!
//! Upstream (Mastodon v4.7.3, `~/fedifedifedi/mastodon`
//! `app/models/media_attachment.rb`): still-image conversion is designed for
//! HEIC/HEIF/AVIF → JPEG only — `IMAGE_CONVERTIBLE_MIME_TYPES` (`:65`) feeds
//! `IMAGE_CONVERTED_STYLES` (`format: 'jpeg'`, `:88-95`) via `file_styles`
//! (`:328-332`) — but the convertible list is currently empty
//! (`IMAGE_CONVERTIBLE_MIME_TYPES = [].freeze`, `:65`) and the accepted
//! image list admits none of them (`IMAGE_MIME_TYPES`, `:64`), both
//! annotated "HEIC/HEIF/AVIF are temporarily disabled for security reasons"
//! (also `:53`). TIFF is accepted nowhere upstream. Birchtree's recorded D4
//! decision differs deliberately: HEIC/HEIF/TIFF are accepted as still
//! images and every still converts to WebP (`-quality 80
//! -compression_level 4`), reusing the Phase 7.1 `ImageToWebp` plan, job,
//! and per-job caps (120 s wall-time / 64 MiB output / 1 GiB RSS).
//!
//! No test needs a real ffmpeg: conversion runs against tiny POSIX `sh`
//! fake-ffmpeg scripts (canned-WebP / failing / copying), following the
//! `tests/phase7_1_transcode.rs` harness pattern. Recorded environment fact:
//! no ffmpeg, ImageMagick, or Pillow exists on this machine, so real
//! HEIC/TIFF fixtures cannot be generated or decoded here — fixtures are
//! hand-crafted magic-byte stills (HEIC/HEIF `ftyp` brands, TIFF `II*\0` /
//! `MM\0*` headers), and the "conversion" is a canned real WebP (encoded
//! in-test with the `image` crate) served by the fake toolchain. A job whose
//! ffmpeg cannot decode the input commits `Failed` (quarantined, surfaced as
//! 422) — never a silent success, never zero-filled bytes.
//!
//! Unix-only (`#![cfg(unix)]` below): the fake-ffmpeg harness shells out to
//! POSIX `sh` scripts with `chmod +x` bits, which have no Windows equivalent.
//! Gating the whole file keeps `cargo check --all-targets` green on Windows
//! with zero behavior change on Linux.

#![cfg(unix)]

mod common;

use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;

use birchtree::application::media::{upload_media, upload_media_with_config, FILE_TYPE_ERROR};
use birchtree::application::ports::{BlobStore, MediaStore, NewMediaUpload, Store};
use birchtree::domain::{Error, Id, MediaState};
use birchtree::media::ingest::{self, ACCEPTED_MIME_TYPES};
use birchtree::media::transcode::{
    build_argv, needs_transcode, plan_for_mime, run_transcode_job, staging_key_for,
    validate_toolchain, TranscodeConfig, TranscodePlan,
};
use common::T0;

// ---------------------------------------------------------------------------
// Fixtures (hand-crafted magic-byte stills; no decoder needed to build them).
// ---------------------------------------------------------------------------

/// Minimal ISO-BMFF bytes with the given major `ftyp` brand (16 bytes, so
/// the sniffer's 12-byte floor is cleared).
fn ftyp_brand(brand: &[u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0u8, 0, 0, 0x18];
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(brand);
    bytes.extend_from_slice(&[0; 4]);
    bytes
}

fn tiff_le_bytes() -> Vec<u8> {
    let mut bytes = vec![0x49, 0x49, 0x2A, 0x00]; // `II*\0`
    bytes.extend_from_slice(&[0; 12]);
    bytes
}

fn tiff_be_bytes() -> Vec<u8> {
    let mut bytes = vec![0x4D, 0x4D, 0x00, 0x2A]; // `MM\0*`
    bytes.extend_from_slice(&[0; 12]);
    bytes
}

/// A real 6x4 WebP, encoded in-test: the canned "conversion output" the fake
/// toolchain serves, so the commit path decodes genuine WebP bytes.
fn canned_webp_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(6, 4, |x, y| {
        image::Rgba([(x * 30) as u8, (y * 40) as u8, 100, 255])
    }));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::WebP)
        .expect("encode canned webp");
    cursor.into_inner()
}

// ---------------------------------------------------------------------------
// Fake-ffmpeg harnesses (POSIX sh; no real ffmpeg needed).
// ---------------------------------------------------------------------------

/// Write an executable `sh` script into `dir` and return its path.
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fake ffmpeg");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path.to_string_lossy().into_owned()
}

const TOOLCHAIN_PREAMBLE: &str = r#"for a in "$@"; do
  case "$a" in
    -version)
      echo "ffmpeg version 9.0.1-fake-test Copyright (c) 2000-2026"
      exit 0
      ;;
    -encoders)
      echo "Encoders:"
      echo " V..... libvpx-vp9           libvpx VP9 (codec vp9)"
      echo " A..... libopus             libopus Opus (codec opus)"
      echo " V..... libwebp             libwebp WebP image (codec webp)"
      exit 0
      ;;
    -decoders)
      echo "Decoders:"
      for d in png mjpeg gif bmp webp tiff h264 hevc mpeg4 vp8 vp9 aac opus vorbis mp3 flac pcm_s16le; do
        echo " V..... $d                  fake decoder"
      done
      exit 0
      ;;
  esac
done
if [ "$#" -eq 3 ]; then
  exit 0
fi
"#;

/// Full fake toolchain whose "transcode" serves the canned WebP at
/// `canned`: answers `-version`/`-encoders`/`-decoders` like a real
/// toolchain (including the `tiff`/`hevc` readiness decoders), probes
/// report nothing, and any transcode copies the canned output last.
fn converting_toolchain(canned: &str) -> String {
    format!(
        "#!/bin/sh\n{TOOLCHAIN_PREAMBLE}out=\"\"\nfor a in \"$@\"; do\n  out=\"$a\"\ndone\ncp \"{canned}\" \"$out\"\n"
    )
}

/// Full fake toolchain whose transcode always exits 1 (deterministic input
/// failure, e.g. a real ffmpeg that cannot decode HEIC without libheif).
const FAILING_TOOLCHAIN: &str = r#"#!/bin/sh
for a in "$@"; do
  case "$a" in
    -version)
      echo "ffmpeg version 9.0.1-fake-test Copyright (c) 2000-2026"
      exit 0
      ;;
    -encoders)
      echo "Encoders:"
      echo " V..... libvpx-vp9           libvpx VP9 (codec vp9)"
      echo " A..... libopus             libopus Opus (codec opus)"
      echo " V..... libwebp             libwebp WebP image (codec webp)"
      exit 0
      ;;
    -decoders)
      echo "Decoders:"
      for d in png mjpeg gif bmp webp tiff h264 hevc mpeg4 vp8 vp9 aac opus vorbis mp3 flac pcm_s16le; do
        echo " V..... $d                  fake decoder"
      done
      exit 0
      ;;
  esac
done
if [ "$#" -eq 3 ]; then
  exit 0
fi
in=""
prev=""
out=""
for a in "$@"; do
  if [ "$prev" = "-i" ]; then in="$a"; fi
  prev="$a"
  out="$a"
done
head -c 16 "$in" > "$out"
echo "Conversion failed: fake decoder cannot decode heic" >&2
exit 1
"#;

/// Full fake toolchain that copies input → output (like `-c copy`): used
/// with a tiny output cap to prove the size cap fires.
const COPY_TOOLCHAIN: &str = r#"#!/bin/sh
for a in "$@"; do
  case "$a" in
    -version)
      echo "ffmpeg version 9.0.1-fake-test Copyright (c) 2000-2026"
      exit 0
      ;;
    -encoders)
      echo "Encoders:"
      echo " V..... libvpx-vp9           libvpx VP9 (codec vp9)"
      echo " A..... libopus             libopus Opus (codec opus)"
      echo " V..... libwebp             libwebp WebP image (codec webp)"
      exit 0
      ;;
    -decoders)
      echo "Decoders:"
      for d in png mjpeg gif bmp webp tiff h264 hevc mpeg4 vp8 vp9 aac opus vorbis mp3 flac pcm_s16le; do
        echo " V..... $d                  fake decoder"
      done
      exit 0
      ;;
  esac
done
if [ "$#" -eq 3 ]; then
  exit 0
fi
in=""
prev=""
out=""
for a in "$@"; do
  if [ "$prev" = "-i" ]; then in="$a"; fi
  prev="$a"
  out="$a"
done
cp "$in" "$out"
"#;

fn config_with(path: &str) -> TranscodeConfig {
    TranscodeConfig {
        ffmpeg_path: path.to_string(),
        ..TranscodeConfig::default()
    }
}

fn test_blobs(dir: &std::path::Path) -> birchtree::media::blob_store::FsBlobStore {
    birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("open blob store")
}

/// Stage `bytes` under a shard key and create a `Pending` row for it.
fn stage_pending(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    account_id: Id,
    key: &str,
    wire_type: &str,
    mime_type: &str,
    bytes: &[u8],
) -> birchtree::domain::MediaAttachment {
    blobs.put(key, bytes).expect("stage original");
    store
        .create_media_upload(
            NewMediaUpload {
                account_id,
                r#type: wire_type.to_string(),
                mime_type: mime_type.to_string(),
                description: String::new(),
                file_size: bytes.len() as u64,
                file_key: key.to_string(),
                thumbnail_key: None,
                focus: None,
                delay: true,
                width: None,
                height: None,
                duration_ms: None,
                frame_rate: None,
                bitrate: None,
            },
            T0,
        )
        .expect("create pending row")
}

fn assert_webp_magic(bytes: &[u8]) {
    assert!(bytes.len() > 12, "derived blob too short for a WebP");
    assert_eq!(&bytes[0..4], b"RIFF", "derived blob is not RIFF");
    assert_eq!(&bytes[8..12], b"WEBP", "derived blob is not WebP");
}

// ---------------------------------------------------------------------------
// 1. Sniffer acceptance.
// ---------------------------------------------------------------------------

#[test]
fn sniffer_accepts_heic_heif_tiff_as_still_images() {
    for (brand, mime, extension) in [
        (*b"heic", "image/heic", "heic"),
        (*b"heix", "image/heic", "heic"),
        (*b"hevc", "image/heic", "heic"),
        (*b"mif1", "image/heic", "heic"),
        (*b"msf1", "image/heic", "heic"),
        (*b"heif", "image/heif", "heif"),
        (*b"heis", "image/heif", "heif"),
    ] {
        let sniffed = ingest::sniff(&ftyp_brand(&brand))
            .unwrap_or_else(|| panic!("brand {} should sniff", String::from_utf8_lossy(&brand)));
        assert_eq!(sniffed.mime_type, mime, "brand {brand:?}");
        assert_eq!(sniffed.r#type, "image", "brand {brand:?} must stay image, never video");
        assert_eq!(sniffed.extension, extension);
        assert!(ingest::validate(sniffed), "brand {brand:?} must be accepted");
        assert!(ingest::is_image(sniffed));
        assert!(!ingest::is_large_format(sniffed));
    }
    for bytes in [tiff_le_bytes(), tiff_be_bytes()] {
        let sniffed = ingest::sniff(&bytes).expect("tiff magic should sniff");
        assert_eq!(sniffed.mime_type, "image/tiff");
        assert_eq!(sniffed.r#type, "image");
        assert_eq!(sniffed.extension, "tif");
        assert!(ingest::validate(sniffed));
        assert!(ingest::is_image(sniffed));
        assert!(!ingest::is_large_format(sniffed));
    }
    // The D4 register names all four spellings.
    for mime in ["image/heic", "image/heif", "image/tiff", "image/x-tiff"] {
        assert!(
            ACCEPTED_MIME_TYPES.contains(&mime),
            "{mime} must be an accepted upload type"
        );
    }
    // Regression: neighboring brands keep their old classification.
    assert_eq!(ingest::sniff(&ftyp_brand(b"isom")).unwrap().mime_type, "video/mp4");
    assert_eq!(ingest::sniff(&ftyp_brand(b"isom")).unwrap().r#type, "video");
    let mut m4a = vec![0u8, 0, 0, 0x18];
    m4a.extend_from_slice(b"ftypM4A ");
    m4a.extend_from_slice(&[0; 4]);
    assert_eq!(ingest::sniff(&m4a).unwrap().mime_type, "audio/mp4");
}

// ---------------------------------------------------------------------------
// 2. Routing to ImageToWebp with the recorded WebP knobs.
// ---------------------------------------------------------------------------

#[test]
fn new_stills_route_to_image_to_webp_with_recorded_knobs() {
    for mime in ["image/heic", "image/heif", "image/tiff", "image/x-tiff"] {
        assert_eq!(plan_for_mime(mime), TranscodePlan::ImageToWebp, "{mime}");
        assert!(needs_transcode(mime), "{mime} must take the transcode path");
    }
    assert_eq!(
        birchtree::media::transcode::derived_extension(TranscodePlan::ImageToWebp),
        "webp"
    );
    // The ffmpeg still-image command reuses the D-decision knobs: libwebp at
    // quality 80, compression_level 4, metadata preserved (never stripped),
    // output path last (fake-ffmpeg harness contract).
    let config = TranscodeConfig::default();
    assert_eq!(config.webp_quality, 80);
    let argv = build_argv(
        "ffmpeg",
        std::path::Path::new("/tmp/in.heic"),
        std::path::Path::new("/tmp/out.webp"),
        TranscodePlan::ImageToWebp,
        false,
        &config,
    );
    let joined = argv.join(" ");
    assert!(joined.contains("libwebp"), "{joined}");
    assert!(joined.contains("-quality 80"), "{joined}");
    assert!(joined.contains("-compression_level 4"), "{joined}");
    assert!(joined.contains("-map_metadata 0"), "{joined}");
    assert!(
        !argv.iter().any(|arg| arg.contains("strip")),
        "no strip filter: {joined}"
    );
    assert_eq!(argv.last().map(String::as_str), Some("/tmp/out.webp"));
}

// ---------------------------------------------------------------------------
// 3. Upload accept + TIFF fixture → WebP bytes (canned toolchain).
// ---------------------------------------------------------------------------

#[test]
fn tiff_upload_defers_then_converges_to_webp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canned = canned_webp_bytes();
    assert_webp_magic(&canned);
    let canned_path = dir.path().join("canned.webp");
    std::fs::write(&canned_path, &canned).expect("write canned webp");
    let fake = write_script(
        dir.path(),
        "ffmpeg",
        &converting_toolchain(&canned_path.to_string_lossy()),
    );
    validate_toolchain(&fake).expect("fake toolchain must validate");
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(8_301);

    // v2-style upload (delay): the TIFF is accepted and stays Pending — the
    // transcode job (post-commit, bounded, idempotent) does the conversion.
    let media = upload_media_with_config(
        &*store,
        &blobs,
        T0,
        owner,
        &tiff_le_bytes(),
        None,
        "",
        None,
        true,
        birchtree::media::transcode::DEFAULT_MAX_FILE_SIZE,
        Some(&config),
    )
    .expect("tiff upload must be accepted");
    assert_eq!(media.mime_type, "image/tiff");
    assert_eq!(media.r#type, "image");
    assert_eq!(media.state, MediaState::Pending);

    run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config).expect("transcode job");

    let ready = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(ready.state, MediaState::Ready);
    assert_eq!(ready.r#type, "image");
    // The derived blob replaced the original at the attachment's own key and
    // is genuine WebP (magic + decodable dims from the derived bytes).
    let derived = blobs.get(&ready.file_key).unwrap().expect("derived blob");
    assert_eq!(derived, canned);
    assert_webp_magic(&derived);
    assert_eq!((ready.width, ready.height), (Some(6), Some(4)));
    assert!(ready.blurhash.as_deref().is_some_and(|b| !b.is_empty()));
    let preview_key = ready.preview_key.clone().expect("small preview staged");
    assert!(preview_key.ends_with(".webp"), "{preview_key}");
    assert_eq!((ready.preview_width, ready.preview_height), (Some(6), Some(4)));
    // No staging orphan is left behind, and replay is a no-op.
    let staging = staging_key_for(&ready.file_key, "webp").expect("staging key");
    assert_eq!(blobs.get(&staging).unwrap(), None);
    run_transcode_job(&*store, &blobs, media.id, T0 + 2, &config).expect("replay");
    let again = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(again.state, MediaState::Ready);
    assert_eq!(blobs.get(&again.file_key).unwrap().as_deref(), Some(canned.as_slice()));
}

#[test]
fn heic_upload_converts_inline_to_webp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canned = canned_webp_bytes();
    let canned_path = dir.path().join("canned.webp");
    std::fs::write(&canned_path, &canned).expect("write canned webp");
    let fake = write_script(
        dir.path(),
        "ffmpeg",
        &converting_toolchain(&canned_path.to_string_lossy()),
    );
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(8_302);

    // v1-style upload (no delay): accepted, converted inline, Ready at once.
    let media = upload_media_with_config(
        &*store,
        &blobs,
        T0,
        owner,
        &ftyp_brand(b"heic"),
        None,
        "",
        None,
        false,
        birchtree::media::transcode::DEFAULT_MAX_FILE_SIZE,
        Some(&config),
    )
    .expect("heic upload must be accepted");
    assert_eq!(media.mime_type, "image/heic");
    assert_eq!(media.r#type, "image");
    assert_eq!(media.state, MediaState::Ready);
    let derived = blobs.get(&media.file_key).unwrap().expect("derived blob");
    assert_webp_magic(&derived);
    assert_eq!((media.width, media.height), (Some(6), Some(4)));
}

// ---------------------------------------------------------------------------
// 4. Failure is quarantined Failed, never silent success / zero-fill.
// ---------------------------------------------------------------------------

#[test]
fn undecodable_heic_fails_loudly_and_keeps_no_partial_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_script(dir.path(), "ffmpeg", FAILING_TOOLCHAIN);
    validate_toolchain(&fake).expect("fake toolchain must validate");
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(8_303);
    let heic = ftyp_brand(b"heic");

    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "hh/ee/cc/original.heic",
        "image",
        "image/heic",
        &heic,
    );
    run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config).expect("job completes");

    // The job itself completes (`Ok`); the ROW is quarantined Failed with a
    // reason the API surfaces as 422 — the failure is loud, not silent.
    let failed = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(failed.state, MediaState::Failed);
    let reason = failed.error.clone().unwrap_or_default();
    assert!(reason.contains("transcode failed"), "{reason}");
    // The staged original is untouched: no partial blob, no zero-fill — the
    // truncated ffmpeg output was deleted, never committed.
    assert_eq!(blobs.get(&failed.file_key).unwrap().as_deref(), Some(heic.as_slice()));
    assert!(failed.blurhash.is_none());
    assert!(failed.preview_key.is_none());
    assert_eq!((failed.width, failed.height), (None, None));
}

#[test]
fn tiff_without_a_toolchain_is_refused_not_silently_served() {
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(8_304);
    // Legacy path (no transcode config): the pure-Rust decoders cannot read
    // TIFF, so the upload is refused with the pinned 422 body — the original
    // is never served as if it had processed.
    let err = upload_media(
        &*store,
        &blobs,
        "https://example.test",
        T0,
        owner,
        &tiff_be_bytes(),
        None,
        "",
        None,
        false,
        birchtree::media::transcode::DEFAULT_MAX_FILE_SIZE,
    )
    .expect_err("toolchain-less tiff upload must be refused");
    assert!(
        matches!(&err, Error::InvalidInput(msg) if msg == FILE_TYPE_ERROR),
        "must be the pinned 422 body, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Per-job caps are enforced on the new stills path.
// ---------------------------------------------------------------------------

#[test]
fn output_cap_breach_on_tiff_fails_the_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_script(dir.path(), "ffmpeg", COPY_TOOLCHAIN);
    let mut config = config_with(&fake);
    // 8 bytes: any real derived WebP breaches it — the cap (default 64 MiB)
    // is enforced on HEIC/TIFF jobs exactly like the existing jobs.
    config.max_output_bytes = 8;
    assert_eq!(config.limits().max_output_bytes, 8);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(8_305);
    // A 16-byte TIFF copies to 16 bytes > the 8-byte cap.
    let tiff = tiff_le_bytes();
    assert!(tiff.len() as u64 > 8);

    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "tt/ii/ff/original.tif",
        "image",
        "image/tiff",
        &tiff,
    );
    run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config).expect("job completes");

    let failed = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(failed.state, MediaState::Failed);
    let reason = failed.error.clone().unwrap_or_default();
    assert!(reason.contains("transcode failed"), "{reason}");
    assert!(reason.contains("8-byte limit"), "{reason}");
}
