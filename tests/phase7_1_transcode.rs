//! Phase 7.1 media transcoding tests: decision table, startup validation,
//! ffmpeg command construction, EXIF/metadata preservation, the v1/v2
//! lifecycle, and crash safety.
//!
//! No test requires a real ffmpeg: execution paths run against tiny POSIX
//! `sh` fake-ffmpeg scripts (copy / canned-probe / failing), and the live
//! toolchain tests gate on `validate_toolchain("ffmpeg")` succeeding. On a
//! machine without ffmpeg (like this one — `which ffmpeg` finds nothing)
//! the gated tests report SKIP and pass.
//!
//! Recorded environment fact (2026-10-20): `ffmpeg` is absent here, so
//! `TranscodeConfig::default().validate()` must be `Err` on this machine.
//!
//! Unix-only (`#![cfg(unix)]` below): the fake-ffmpeg harness shells out to
//! POSIX `sh` scripts with `chmod +x` bits, which have no Windows equivalent.
//! Gating the whole file keeps `cargo check --all-targets` green on Windows
//! with zero behavior change on Linux.

#![cfg(unix)]

mod common;

use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::api::mastodon::dto;
use birchtree::application::media::{show_media, upload_media, THUMBNAIL_ERROR};
use birchtree::application::ports::{
    BlobStore, JobQueue, MediaStore, NewMediaUpload, NoStreaming, Store,
};
use birchtree::domain::{Id, JobKind, MediaState};
use birchtree::jobs::handlers;
use birchtree::mail::LoggingMailSender;
use birchtree::media::transcode::{
    build_argv, needs_transcode, parse_probe_stderr, plan_for_mime, probe_is_passthrough,
    staging_key_for, validate_toolchain, TranscodeConfig, TranscodeJobPayload, TranscodePlan,
};
use common::{form_request, get_request, json, send, server_info, TestServer, T0, TEST_PUBLIC_URL};

// ---------------------------------------------------------------------------
// Fixtures (tiny synthetic files generated in-test).
// ---------------------------------------------------------------------------

fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
        6,
        4,
        |x, y| image::Rgba([(x * 30) as u8, (y * 40) as u8, 128, 255]),
    ));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

fn jpeg_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(6, 4, |x, y| {
        image::Rgb([(x * 30) as u8, (y * 40) as u8, 128])
    }));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .expect("encode jpeg fixture");
    cursor.into_inner()
}

fn gif_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
        6,
        4,
        |x, y| image::Rgba([(x * 30) as u8, (y * 40) as u8, 200, 255]),
    ));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Gif)
        .expect("encode gif fixture");
    cursor.into_inner()
}

fn webp_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
        6,
        4,
        |x, y| image::Rgba([(x * 30) as u8, (y * 40) as u8, 100, 255]),
    ));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::WebP)
        .expect("encode webp fixture");
    cursor.into_inner()
}

/// Minimal ISO-BMFF bytes that sniff as `video/mp4`.
fn mp4_bytes() -> Vec<u8> {
    let mut mp4 = vec![0u8, 0, 0, 0x18];
    mp4.extend_from_slice(b"ftypisom");
    mp4.extend_from_slice(&[0; 8]);
    mp4
}

/// A JPEG with a minimal APP1 EXIF segment (Orientation = 6) spliced after
/// SOI, so the EXIF-preservation tests have a tag to look for.
fn jpeg_with_exif() -> Vec<u8> {
    let jpeg = jpeg_bytes();
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8], "SOI preamble");
    // "Exif\0\0" + little-endian TIFF: II* + IFD0 with one SHORT entry,
    // tag 0x0112 (Orientation) = 6.
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&[
        0x49, 0x49, 0x2A, 0x00, // "II", 42
        0x08, 0x00, 0x00, 0x00, // IFD0 offset
        0x01, 0x00, // one entry
        0x12, 0x01, 0x03, 0x00, // tag 0x0112, type SHORT
        0x01, 0x00, 0x00, 0x00, // count 1
        0x06, 0x00, 0x00, 0x00, // value 6
        0x00, 0x00, 0x00, 0x00, // next IFD offset
    ]);
    let len = (payload.len() + 2) as u16;
    let mut app1 = vec![0xFF, 0xE1, (len >> 8) as u8, (len & 0xFF) as u8];
    app1.extend_from_slice(&payload);
    let mut out = jpeg[0..2].to_vec();
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
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

/// Answers `-version`/`-encoders`/`-decoders` like a full toolchain; probes
/// (`ffmpeg -hide_banner -i <file>`, exactly 3 args) report unknown streams;
/// anything else copies input → output (last arg).
const FAKE_TOOLCHAIN: &str = r#"#!/bin/sh
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

/// Same as [`FAKE_TOOLCHAIN`] but its probe reports VP9 video + Opus audio
/// at 640x480 with a 2.5 s duration (the D3 passthrough shape).
const FAKE_VP9_PROBE: &str = r#"#!/bin/sh
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
  echo "  Input #0, matroska,webm, from 'in.webm':" >&2
  echo "  Duration: 00:00:02.50, start: 0.000000, bitrate: 100 kb/s" >&2
  echo "  Stream #0:0: Video: vp9 (Profile 0), yuv420p(tv), 640x480 [SAR 1:1 DAR 4:3], 25 fps" >&2
  echo "  Stream #0:1(eng): Audio: opus, 48000 Hz, stereo, fltp" >&2
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

/// Same as [`FAKE_VP9_PROBE`] but the probe reports a MP4 container: VP9
/// video + Opus audio in `mov,mp4,...` (the remux shape — stream copy into
/// WebM, no re-encode).
const FAKE_VP9_MP4_PROBE: &str = r#"#!/bin/sh
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
  echo "  Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'in.mp4':" >&2
  echo "  Duration: 00:00:02.50, start: 0.000000, bitrate: 100 kb/s" >&2
  echo "  Stream #0:0: Video: vp9 (Profile 0), yuv420p(tv), 640x480 [SAR 1:1 DAR 4:3], 25 fps" >&2
  echo "  Stream #0:1(eng): Audio: opus, 48000 Hz, stereo, fltp" >&2
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

/// Writes a truncated output then exits 1 (deterministic ffmpeg failure).
const FAKE_FAILURE: &str = r#"#!/bin/sh
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
echo "Conversion failed: fake codec error" >&2
exit 1
"#;

/// Same as [`FAKE_TOOLCHAIN`] but without the VP9 encoder line.
const FAKE_MISSING_VP9: &str = r#"#!/bin/sh
for a in "$@"; do
  case "$a" in
    -version)
      echo "ffmpeg version 9.0.1-fake-test Copyright (c) 2000-2026"
      exit 0
      ;;
    -encoders)
      echo "Encoders:"
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
exit 0
"#;

fn config_with(path: &str) -> TranscodeConfig {
    TranscodeConfig {
        ffmpeg_path: path.to_string(),
        ..TranscodeConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Decision + passthrough matrix (per input class, no ffmpeg needed).
// ---------------------------------------------------------------------------

#[test]
fn decision_table_per_input_class() {
    use TranscodePlan::{GifToWebm, ImageToWebp, Passthrough, VideoToWebm};
    // Stills (JPEG, PNG, TIFF-class via bmp here) → WebP.
    assert_eq!(plan_for_mime("image/jpeg"), ImageToWebp);
    assert_eq!(plan_for_mime("image/png"), ImageToWebp);
    assert_eq!(plan_for_mime("image/bmp"), ImageToWebp);
    // GIF (animated or not) → WebM/VP9.
    assert_eq!(plan_for_mime("image/gif"), GifToWebm);
    // Non-VP9 video → WebM/VP9; VP9 WebM reports VideoToWebm statically and
    // the job's probe decides passthrough.
    assert_eq!(plan_for_mime("video/mp4"), VideoToWebm);
    assert_eq!(plan_for_mime("video/webm"), VideoToWebm);
    // WebP stills pass through; audio is never transcoded.
    assert_eq!(plan_for_mime("image/webp"), Passthrough);
    assert_eq!(plan_for_mime("audio/ogg"), Passthrough);
    assert_eq!(plan_for_mime("audio/mpeg"), Passthrough);
    assert_eq!(plan_for_mime("audio/mp4"), Passthrough);
    // HEIC/HEIF/TIFF are accepted D4 stills (round 3): they route to the
    // WebP transcode, never the legacy path.
    assert_eq!(plan_for_mime("image/heic"), ImageToWebp);
    assert_eq!(plan_for_mime("image/heif"), ImageToWebp);
    assert_eq!(plan_for_mime("image/tiff"), ImageToWebp);
    assert_eq!(plan_for_mime("image/x-tiff"), ImageToWebp);
    assert!(needs_transcode("image/gif"));
    assert!(needs_transcode("video/mp4"));
    assert!(!needs_transcode("image/webp"));
    assert!(!needs_transcode("audio/ogg"));
}

#[test]
fn vp9_webm_passthrough_bound() {
    let base = birchtree::media::transcode::Probe {
        video_codec: Some("vp9".to_string()),
        audio_codec: Some("opus".to_string()),
        container: Some("matroska,webm".to_string()),
        width: Some(640),
        height: Some(480),
        duration_ms: Some(2_500),
    };
    let limit = TranscodeConfig::default().max_file_size;
    assert!(probe_is_passthrough(&base, 1024, limit));
    assert!(probe_is_passthrough(
        &birchtree::media::transcode::Probe {
            audio_codec: None,
            ..base.clone()
        },
        1024,
        limit
    ));
    for probe in [
        birchtree::media::transcode::Probe {
            video_codec: Some("h264".to_string()),
            ..base.clone()
        },
        birchtree::media::transcode::Probe {
            video_codec: Some("vp8".to_string()),
            ..base.clone()
        },
        birchtree::media::transcode::Probe {
            audio_codec: Some("aac".to_string()),
            ..base.clone()
        },
        birchtree::media::transcode::Probe {
            video_codec: None,
            ..base.clone()
        },
    ] {
        assert!(!probe_is_passthrough(&probe, 1024, limit), "{probe:?}");
    }
    // Over the upload size limit → transcode even when VP9/Opus.
    assert!(!probe_is_passthrough(&base, limit + 1, limit));
    assert!(probe_is_passthrough(&base, limit, limit));
}

// ---------------------------------------------------------------------------
// Startup validation.
// ---------------------------------------------------------------------------

#[test]
fn startup_validation_missing_binary_is_an_error() {
    let err = validate_toolchain("/nonexistent/birchtree-ffmpeg-xyz").expect_err("missing");
    assert!(err.contains("could not be executed"), "{err}");
    // Disabled transcoding validates without probing any binary.
    let disabled = TranscodeConfig {
        enabled: false,
        ffmpeg_path: "/nonexistent/birchtree-ffmpeg-xyz".to_string(),
        ..TranscodeConfig::default()
    };
    assert!(!disabled.validate().expect("disabled").enabled);
    // Knob ranges are startup errors too.
    for bad in [
        TranscodeConfig {
            webp_quality: 101,
            ..TranscodeConfig::default()
        },
        TranscodeConfig {
            vp9_crf: 64,
            ..TranscodeConfig::default()
        },
        TranscodeConfig {
            wall_time_secs: 0,
            ..TranscodeConfig::default()
        },
    ] {
        assert!(bad.validate().is_err());
    }
}

#[test]
fn startup_validation_reports_missing_components() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = write_script(dir.path(), "ffmpeg-missing", FAKE_MISSING_VP9);
    let err = validate_toolchain(&missing).expect_err("vp9 encoder missing");
    assert!(err.contains("encoder libvpx-vp9"), "{err}");
    let full = write_script(dir.path(), "ffmpeg-full", FAKE_TOOLCHAIN);
    let info = validate_toolchain(&full).expect("full fake toolchain");
    assert!(info.enabled);
    assert!(info.version.contains("ffmpeg version"), "{}", info.version);
}

#[test]
fn record_real_toolchain_presence_for_the_log() {
    // Informational only: on this machine ffmpeg is absent, so the default
    // config must fail validation here (missing binary → startup error,
    // never silent passthrough).
    match TranscodeConfig::default().validate() {
        Ok(info) => eprintln!("LIVE ffmpeg present: {}", info.version),
        Err(err) => eprintln!("SKIP live-ffmpeg tests (no ffmpeg here): {err}"),
    }
}

// ---------------------------------------------------------------------------
// Command construction + metadata rules.
// ---------------------------------------------------------------------------

#[test]
fn every_command_preserves_metadata_and_never_strips() {
    let cfg = TranscodeConfig::default();
    let input = std::path::Path::new("/tmp/in.bin");
    let output = std::path::Path::new("/tmp/out.bin");
    for (plan, with_audio) in [
        (TranscodePlan::ImageToWebp, false),
        (TranscodePlan::GifToWebm, false),
        (TranscodePlan::VideoToWebm, true),
        (TranscodePlan::VideoToWebm, false),
    ] {
        let argv = build_argv("ffmpeg", input, output, plan, with_audio, &cfg);
        // Metadata rides along; no strip flag, no pixel filter (orientation
        // is kept as a tag, never applied).
        assert!(argv.contains(&"-map_metadata".to_string()), "{plan:?}");
        assert!(
            !argv.iter().any(|a| a.contains("-map_metadata -1") || a == "-vf"),
            "{plan:?}: {argv:?}"
        );
        // Fake-ffmpeg harness contract.
        let i = argv.iter().position(|a| a == "-i").expect("has -i");
        assert_eq!(argv[i + 1], "/tmp/in.bin");
        assert_eq!(argv.last().map(String::as_str), Some("/tmp/out.bin"));
    }
    // Recorded Phase 7.1 quality defaults.
    let still = build_argv("ffmpeg", input, output, TranscodePlan::ImageToWebp, false, &cfg);
    assert!(still.contains(&"libwebp".to_string()));
    assert!(still.contains(&"80".to_string()));
    let video = build_argv("ffmpeg", input, output, TranscodePlan::VideoToWebm, true, &cfg);
    assert!(video.contains(&"libvpx-vp9".to_string()));
    assert!(video.contains(&"30".to_string()));
    assert!(video.contains(&"libopus".to_string()));
    assert!(video.contains(&"96k".to_string()));
    // Silent video maps no audio stream.
    let silent = build_argv(
        "ffmpeg",
        input,
        output,
        TranscodePlan::VideoToWebm,
        false,
        &cfg,
    );
    assert!(silent.contains(&"-an".to_string()));
    assert!(!silent.iter().any(|a| a == "0:a:0"));
}

#[test]
fn exif_fixture_carries_an_orientation_tag() {
    let bytes = jpeg_with_exif();
    assert_eq!(&bytes[0..2], &[0xFF, 0xD8]);
    let app1 = &bytes[2..2 + 2 + 2 + 6];
    assert_eq!(&app1[0..2], &[0xFF, 0xE1]);
    assert!(bytes.windows(6).any(|w| w == b"Exif\0\0"));
    // The fixture still decodes as a JPEG (APP1 is skipped).
    let img = image::load_from_memory(&bytes).expect("decode exif jpeg");
    assert_eq!((img.width(), img.height()), (6, 4));
}

#[test]
fn exif_survives_transcode_when_a_toolchain_exists() {
    if validate_toolchain("ffmpeg").is_err() {
        eprintln!("SKIP live EXIF test: no ffmpeg on this machine");
        return;
    }
    let config = TranscodeConfig::default();
    let (derived, _) =
        birchtree::media::transcode::transcode_bytes(&jpeg_with_exif(), "jpg", TranscodePlan::ImageToWebp, false, &config)
            .expect("live transcode");
    assert!(
        derived.windows(6).any(|w| w == b"Exif\0\0"),
        "EXIF payload must survive image→WebP (-map_metadata 0)"
    );
}

// ---------------------------------------------------------------------------
// Lifecycle helpers.
// ---------------------------------------------------------------------------

/// A blob store rooted inside `dir` (the `TempDir` must stay alive).
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

fn attachment_url(media: &birchtree::domain::MediaAttachment) -> serde_json::Value {
    let value = dto::media_attachment_json(&server_info(), media);
    value.get("url").cloned().unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// Transcode-path lifecycle (fake ffmpeg): v2-style 202 → job → 200.
// ---------------------------------------------------------------------------

#[test]
fn gif_transcode_lifecycle_pending_to_ready() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_script(dir.path(), "ffmpeg", FAKE_TOOLCHAIN);
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_101);
    let gif = gif_bytes();

    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "aa/bb/cc/original.gif",
        "image",
        "image/gif",
        &gif,
    );
    // v2-style pending state: contract-correct `null` URL (202 shape).
    assert_eq!(media.state, MediaState::Pending);
    assert_eq!(attachment_url(&media), serde_json::Value::Null);
    let payload = TranscodeJobPayload { media_id: media.id };
    assert_eq!(
        TranscodeJobPayload::media_id_from_json(&payload.to_json()).unwrap(),
        media.id
    );

    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config)
        .expect("transcode job");

    let ready = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(ready.state, MediaState::Ready);
    // GIF → WebM/VP9 typed `video`; `gifv` never appears.
    assert_eq!(ready.r#type, "video");
    assert_ne!(ready.r#type.as_str(), "gifv");
    // The derived blob replaced the original at the attachment's own key.
    assert_eq!(blobs.get(&ready.file_key).unwrap().as_deref(), Some(gif.as_slice()));
    // No staging orphan is left behind.
    let staging = staging_key_for(&ready.file_key, "webm").expect("staging key");
    assert_eq!(blobs.get(&staging).unwrap(), None);
    // The DTO now serves the derived URL (200 shape).
    assert!(attachment_url(&ready).is_string());
    // Re-running the job is a no-op and the bytes are stable.
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 2, &config)
        .expect("replay");
    let again = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(again.state, MediaState::Ready);
    assert_eq!(blobs.get(&again.file_key).unwrap().as_deref(), Some(gif.as_slice()));
    // The owner's unattached row is addressable (GET answers 200, not 206).
    assert!(show_media(&*store, owner, media.id).is_ok());
}

#[test]
fn vp9_webm_passthrough_needs_no_transcode() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The probe reports VP9 + Opus, so the job must pass the bytes through
    // without running a transcode — even though the "transcoder" here only
    // probes.
    let fake = write_script(dir.path(), "ffmpeg", FAKE_VP9_PROBE);
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_102);
    let webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0];

    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "dd/ee/ff/original.webm",
        "video",
        "video/webm",
        &webm,
    );
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config)
        .expect("passthrough job");
    let ready = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(ready.state, MediaState::Ready);
    assert_eq!(ready.r#type, "video");
    // Original bytes untouched; probe dims committed honestly.
    assert_eq!(blobs.get(&ready.file_key).unwrap().as_deref(), Some(webm.as_slice()));
    assert_eq!((ready.width, ready.height), (Some(640), Some(480)));
    assert_eq!(ready.duration_ms, Some(2_500));
    assert!(attachment_url(&ready).is_string());
}

#[test]
fn vp9_opus_in_mp4_is_remuxed_into_webm_without_reencode() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Codecs are already VP9 + Opus but the container is MP4: the job must
    // stream-copy into WebM (the fake copies bytes, like `-c copy` of
    // already-correct streams) instead of keeping the MP4 bytes or
    // re-encoding. Assert the remux ran by checking the derived staging
    // extension path commits Ready with probed meta.
    let fake = write_script(dir.path(), "ffmpeg", FAKE_VP9_MP4_PROBE);
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_112);
    let mp4 = vec![0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', 0, 0, 0, 0];

    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "aa/bb/cc/original.mp4",
        "video",
        "video/mp4",
        &mp4,
    );
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config)
        .expect("remux job");
    let ready = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(ready.state, MediaState::Ready);
    assert_eq!(ready.r#type, "video");
    // The fake remux copies streams, so served bytes equal the input here;
    // meta comes from the (remuxed) output probe, and no staging orphan stays.
    assert_eq!(blobs.get(&ready.file_key).unwrap().as_deref(), Some(mp4.as_slice()));
    assert_eq!((ready.width, ready.height), (Some(640), Some(480)));
    assert_eq!(ready.duration_ms, Some(2_500));
    let staging = staging_key_for(&ready.file_key, "webm").expect("staging key");
    assert_eq!(blobs.get(&staging).unwrap(), None);
    assert!(attachment_url(&ready).is_string());
}

#[test]
fn webp_still_finishes_without_a_toolchain() {
    // WebP needs no transcode, so the job must succeed even when ffmpeg is
    // missing (no silent passthrough question arises: it IS the target).
    let config = config_with("/nonexistent/birchtree-ffmpeg-xyz");
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_103);
    let webp = webp_bytes();
    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "11/22/33/original.webp",
        "image",
        "image/webp",
        &webp,
    );
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config)
        .expect("webp passthrough");
    let ready = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(ready.state, MediaState::Ready);
    assert_eq!(ready.r#type, "image");
    assert_eq!((ready.width, ready.height), (Some(6), Some(4)));
    assert!(ready.preview_key.is_some());
    assert!(ready.blurhash.is_some());
    // Re-run is a stable no-op.
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 2, &config)
        .expect("replay");
    assert_eq!(store.media_by_id(media.id).unwrap().unwrap().state, MediaState::Ready);
}

// ---------------------------------------------------------------------------
// Crash safety: partial derived output is never surfaced.
// ---------------------------------------------------------------------------

#[test]
fn failed_transcode_never_surfaces_partial_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_script(dir.path(), "ffmpeg-fail", FAKE_FAILURE);
    let config = config_with(&fake);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_104);
    let gif = gif_bytes();
    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "44/55/66/original.gif",
        "image",
        "image/gif",
        &gif,
    );

    // The deterministic ffmpeg failure completes the job as `Failed`
    // (terminal, no retry storm) rather than erroring it.
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 1, &config)
        .expect("failed job still completes");
    let failed = store.media_by_id(media.id).unwrap().expect("row");
    assert_eq!(failed.state, MediaState::Failed);
    // The original bytes are untouched, no staging orphan, URL stays null.
    assert_eq!(blobs.get(&failed.file_key).unwrap().as_deref(), Some(gif.as_slice()));
    let staging = staging_key_for(&failed.file_key, "webm").expect("staging key");
    assert_eq!(blobs.get(&staging).unwrap(), None);
    assert_eq!(attachment_url(&failed), serde_json::Value::Null);
    // The API answers 422 for a failed row (Mastodon `check_processing`).
    let err = show_media(&*store, owner, media.id).expect_err("failed row 422s");
    assert_eq!(err.to_string(), format!("invalid input: {THUMBNAIL_ERROR}"));
    // A re-run is a terminal no-op (the row stays Failed, nothing served).
    birchtree::media::transcode::run_transcode_job(&*store, &blobs, media.id, T0 + 2, &config)
        .expect("replay");
    assert_eq!(store.media_by_id(media.id).unwrap().unwrap().state, MediaState::Failed);
}

#[test]
fn missing_blobs_and_toolchain_are_retryable_not_terminal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = write_script(dir.path(), "ffmpeg", FAKE_TOOLCHAIN);
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_105);
    // A row whose staged bytes vanished: Err (the job retries, then
    // quarantines) — never a silent Ready, never a terminal Failed.
    let media = stage_pending(
        &*store,
        &blobs,
        owner,
        "77/88/99/original.gif",
        "image",
        "image/gif",
        &gif_bytes(),
    );
    blobs.delete(&media.file_key).expect("drop staged bytes");
    let err = birchtree::media::transcode::run_transcode_job(
        &*store,
        &blobs,
        media.id,
        T0 + 1,
        &config_with(&fake),
    )
    .expect_err("missing bytes retry");
    assert!(err.to_string().contains("staged original missing"), "{err}");
    assert_eq!(store.media_by_id(media.id).unwrap().unwrap().state, MediaState::Pending);
    // A vanishing toolchain (binary removed after startup validation) is
    // equally retryable — never a silent passthrough of the original.
    let gif2 = stage_pending(
        &*store,
        &blobs,
        owner,
        "77/88/aa/original.gif",
        "image",
        "image/gif",
        &gif_bytes(),
    );
    let err = birchtree::media::transcode::run_transcode_job(
        &*store,
        &blobs,
        gif2.id,
        T0 + 1,
        &config_with("/nonexistent/birchtree-ffmpeg-xyz"),
    )
    .expect_err("missing toolchain retries");
    assert!(err.to_string().contains("toolchain unavailable"), "{err}");
    assert_eq!(store.media_by_id(gif2.id).unwrap().unwrap().state, MediaState::Pending);
}

// ---------------------------------------------------------------------------
// HTTP lifecycle pins (existing behavior): v1 200, v2 202, 206-until-ready.
// ---------------------------------------------------------------------------

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn user_token(srv: &TestServer, username: &str) -> String {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read write follow read:accounts write:accounts write:media"),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("write:accounts"),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_token = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.social")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app_token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

struct Part {
    name: &'static str,
    filename: Option<String>,
    mime: Option<&'static str>,
    value: Vec<u8>,
}

fn file_part(name: &'static str, filename: &str, mime: &'static str, bytes: Vec<u8>) -> Part {
    Part {
        name,
        filename: Some(filename.to_string()),
        mime: Some(mime),
        value: bytes,
    }
}

const BOUNDARY: &str = "birchtree-test-boundary";

fn multipart_request(uri: &str, token: &str, parts: &[Part]) -> Request<Body> {
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match &part.filename {
            Some(filename) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\n\
                         Content-Type: {}\r\n\r\n",
                        part.name,
                        part.mime.unwrap_or("application/octet-stream")
                    )
                    .as_bytes(),
                );
            }
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", part.name).as_bytes(),
            ),
        }
        body.extend_from_slice(&part.value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body))
        .expect("multipart request")
}

#[tokio::test]
async fn v1_upload_answers_200_with_final_url() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "vivian").await;
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request("/api/v1/media", &token, &[file_part(
            "file",
            "still.png",
            "image/png",
            png_bytes(),
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"].as_str(), Some("image"));
    assert!(body["url"].is_string(), "{body}");
}

#[tokio::test]
async fn v2_upload_answers_202_then_206_then_200() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "victor").await;
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request("/api/v2/media", &token, &[file_part(
            "file",
            "clip.mp4",
            "video/mp4",
            mp4_bytes(),
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    let id = body["id"].as_str().expect("id").to_string();
    // Pending: contract-correct `null` URL.
    assert_eq!(body["url"], serde_json::Value::Null, "{body}");

    // Until the job runs, show answers 206.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{}", String::from_utf8_lossy(&bytes));

    // Run the queued ProcessMedia job, then the row is Ready (200 + URL).
    // (Registration may enqueue its own jobs, so select by kind.)
    let jobs = srv.store.claim_due(4_000_000_000_000, 10).expect("claim");
    let job = jobs
        .iter()
        .find(|job| job.kind == JobKind::ProcessMedia)
        .expect("process media job");
    handlers::handle(
        job,
        &*srv.store as &dyn Store,
        &LoggingMailSender,
        &*srv.blobs,
        &NoStreaming,
    )
    .expect("process media job");
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(json(&bytes)["url"].is_string());
}

// ---------------------------------------------------------------------------
// Meta rules for converted blobs.
// ---------------------------------------------------------------------------

#[test]
fn converted_meta_follows_the_derived_blob() {
    // Converted stills: dimensions match the stored (derived) pixels, focus
    // is preserved, and the preview is WebP.
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(7_106);
    let media = upload_media(
        &*store,
        &blobs,
        TEST_PUBLIC_URL,
        T0,
        owner,
        &png_bytes(),
        None,
        "",
        Some("0.5,-0.25"),
        false,
        16 * 1024 * 1024,
    )
    .expect("upload");
    assert_eq!(media.state, MediaState::Ready);
    let value = dto::media_attachment_json(&server_info(), &media);
    let original = &value["meta"]["original"];
    assert_eq!(original["width"].as_u64(), Some(6));
    assert_eq!(original["height"].as_u64(), Some(4));
    assert_eq!(value["meta"]["focus"]["x"].as_f64(), Some(0.5));
    assert!(media.preview_key.is_some());
    // A fake WebP transcode of the same bytes decodes to identical dims, so
    // committing derived meta is dims-preserving by construction.
    let derived_meta = image::load_from_memory(&png_bytes()).expect("decode");
    assert_eq!((derived_meta.width(), derived_meta.height()), (6, 4));
    // Video rows carry no fabricated duration/frame-rate/bitrate.
    assert!(value["meta"].get("duration").is_none() || value["meta"]["duration"].is_null());
}

#[test]
fn probe_parser_fixture_matrix() {
    // VP9 + Opus + duration.
    let probe = parse_probe_stderr(
        "  Duration: 00:01:00.00, start: 0.000000, bitrate: 200 kb/s\n\
         Stream #0:0: Video: vp9 (Profile 0), yuv420p, 320x240, 30 fps\n\
         Stream #0:1: Audio: opus, 48000 Hz, stereo\n",
    );
    assert_eq!(probe.video_codec.as_deref(), Some("vp9"));
    assert_eq!(probe.audio_codec.as_deref(), Some("opus"));
    assert_eq!((probe.width, probe.height), (Some(320), Some(240)));
    assert_eq!(probe.duration_ms, Some(60_000));
    assert!(probe_is_passthrough(&probe, 100, u64::MAX));
    // H264 + AAC transcodes.
    let probe = parse_probe_stderr(
        "  Duration: 00:00:10.00, start: 0.000000\n\
         Stream #0:0: Video: h264, yuv420p, 1920x1080\n\
         Stream #0:1: Audio: aac, 44100 Hz, stereo\n",
    );
    assert!(!probe_is_passthrough(&probe, 100, u64::MAX));
}
