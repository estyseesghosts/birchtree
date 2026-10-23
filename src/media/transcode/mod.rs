//! Media transcoding (Phase 7.1): ffmpeg-backed post-commit job.
//!
//! Decision table (decided 2026-10-17, `birchtree.md` §5 "Media transcoding"
//! and §7 "Phase 7.1"):
//!
//! - GIF (animated or not) → WebM with VP9, wire type `video`. `gifv` is
//!   never produced (deviations D1, D2).
//! - Video → WebM with VP9 and Opus audio (D3). Passthrough bound is
//!   codec-based (like upstream's `eligible_to_passthrough?`, which checks
//!   codecs, never the container): VP9 video with Opus audio or no audio
//!   within the upload size limit keeps its codecs — byte-identical when
//!   already WebM, stream-copied (`-c copy`, no re-encode) into WebM
//!   otherwise, so served bytes are always WebM.
//! - Still images (JPEG, PNG, BMP, HEIC/HEIF, TIFF, ...) → WebP (D4).
//!   WebP passes through. Upstream converts only HEIC, HEIF and AVIF to
//!   JPEG (`IMAGE_CONVERTIBLE_MIME_TYPES` → `IMAGE_CONVERTED_STYLES`,
//!   `media_attachment.rb:65,88-95,328-332`) and currently accepts none of
//!   them (`IMAGE_MIME_TYPES`, `:53,64-65`); birchtree accepts HEIC/HEIF/TIFF
//!   and converts every still to WebP instead.
//!   The generated `small` preview is WebP too.
//! - Audio is not transcoded and keeps the legacy processing path.
//! - Metadata is never stripped: every ffmpeg command carries
//!   `-map_metadata 0` and no strip/normalize filter. Orientation is kept as
//!   a tag, never applied to the pixels.
//!
//! Architecture rules (`birchtree.md` §5):
//!
//! - No ffmpeg or outbound I/O inside a request transaction. Transcoding is a
//!   post-commit durable job: the upload commits metadata + job atomically,
//!   the job runs ffmpeg outside any transaction, stages the derived blob,
//!   and commits the result last.
//! - Crash safety: ffmpeg writes to a temp file outside the blob store; the
//!   derived bytes are staged under a deterministic sibling key, read back
//!   and verified, then written over the attachment's own `file_key` (the
//!   derived blob *replaces* the original, so no metadata pointer swap and no
//!   orphan accumulation), and the row commits `Ready` last. A crash before
//!   commit leaves the row `Pending` (DTO `url` stays `null`, never a
//!   partial blob) and a bounded re-run converges: ffmpeg sniffs content, so
//!   re-transcoding already-derived bytes is still valid output.
//! - Idempotency: a `Ready`/`Failed`/missing row is a no-op. Storage faults
//!   (`get`/`put`) return `Err` so the job retries up to its budget and then
//!   quarantines; deterministic input failures (bad decode, ffmpeg error,
//!   wall-time/output-size/memory limit breach) commit `Failed`, which the
//!   API surfaces as `422`.
//! - A missing or non-working ffmpeg binary is a startup configuration error
//!   (see [`validate_toolchain`]), never a silent passthrough. When
//!   transcoding is enabled, a row that needs transcode and has no working
//!   toolchain fails loudly instead of serving the original.
//!

mod argv;
mod commit;
mod commit_meta;
mod config;
mod job;
mod plan;
mod probe;
mod run;
mod temp;
mod toolchain;

pub use argv::build_argv;
pub use commit::TranscodeCtx;
pub use config::{
    DEFAULT_MAX_FILE_SIZE, DEFAULT_MAX_MEMORY_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_WALL_TIME_SECS, REQUIRED_DECODERS, REQUIRED_ENCODERS, TranscodeConfig, TranscodeLimits,
};
pub use job::{TranscodeJobPayload, run_transcode_job, transcode_bytes};
pub use plan::{TranscodePlan, derived_extension, needs_transcode, plan_for_mime};
pub use probe::{Probe, parse_duration_ms, parse_probe_stderr, probe_file, probe_is_passthrough};
pub use run::{RunStats, TranscodeError};
pub use temp::{
    TEMP_PREFIX, TMP_SWEEP_CAP, TMP_SWEEP_GRACE_MS, staging_key_for, sweep_staging_for_media,
    sweep_tmp_files,
};
pub use toolchain::{ToolchainInfo, validate_toolchain};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::domain::Id;

    fn config_for(path: &str) -> TranscodeConfig {
        TranscodeConfig {
            ffmpeg_path: path.to_string(),
            ..TranscodeConfig::default()
        }
    }

    #[test]
    fn decision_table_covers_every_input_class() {
        use TranscodePlan::{GifToWebm, ImageToWebp, Passthrough, VideoToWebm};
        assert_eq!(plan_for_mime("image/jpeg"), ImageToWebp);
        assert_eq!(plan_for_mime("image/png"), ImageToWebp);
        assert_eq!(plan_for_mime("image/bmp"), ImageToWebp);
        // D4 extension: HEIC/HEIF/TIFF stills → WebP (upstream converts only
        // HEIC/HEIF/AVIF to JPEG and currently accepts none of them).
        assert_eq!(plan_for_mime("image/heic"), ImageToWebp);
        assert_eq!(plan_for_mime("image/heif"), ImageToWebp);
        assert_eq!(plan_for_mime("image/tiff"), ImageToWebp);
        assert_eq!(plan_for_mime("image/x-tiff"), ImageToWebp);
        assert_eq!(plan_for_mime("image/gif"), GifToWebm);
        assert_eq!(plan_for_mime("image/webp"), Passthrough);
        assert_eq!(plan_for_mime("video/mp4"), VideoToWebm);
        assert_eq!(plan_for_mime("video/webm"), VideoToWebm);
        // Audio and unknown types never transcode.
        assert_eq!(plan_for_mime("audio/ogg"), Passthrough);
        assert_eq!(plan_for_mime("audio/mpeg"), Passthrough);
        assert_eq!(plan_for_mime("application/pdf"), Passthrough);
        assert!(needs_transcode("image/gif"));
        assert!(needs_transcode("image/jpeg"));
        assert!(needs_transcode("image/heic"));
        assert!(needs_transcode("image/heif"));
        assert!(needs_transcode("image/tiff"));
        assert!(needs_transcode("video/mp4"));
        assert!(!needs_transcode("image/webp"));
        assert!(!needs_transcode("audio/ogg"));
        assert_eq!(derived_extension(ImageToWebp), "webp");
        assert_eq!(derived_extension(GifToWebm), "webm");
        assert_eq!(derived_extension(VideoToWebm), "webm");
    }

    #[test]
    fn passthrough_bound_needs_vp9_opus_or_silent_within_size() {
        let base = Probe {
            video_codec: Some("vp9".to_string()),
            audio_codec: Some("opus".to_string()),
            container: Some("matroska,webm".to_string()),
            width: Some(640),
            height: Some(480),
            duration_ms: Some(2_500),
        };
        assert!(probe_is_passthrough(&base, 1024, DEFAULT_MAX_FILE_SIZE));
        assert!(base.container_is_webm());
        // The codec bound ignores the container: VP9+Opus in MP4 passes the
        // codec check (the job remuxes it into WebM with stream copy).
        let mp4 = Probe {
            container: Some("mov,mp4,m4a,3gp,3g2,mj2".to_string()),
            ..base.clone()
        };
        assert!(probe_is_passthrough(&mp4, 1024, DEFAULT_MAX_FILE_SIZE));
        assert!(!mp4.container_is_webm());
        // Silent is fine.
        let silent = Probe {
            audio_codec: None,
            ..base.clone()
        };
        assert!(probe_is_passthrough(&silent, 1024, DEFAULT_MAX_FILE_SIZE));
        // Non-VP9 video, non-Opus audio, and oversize files transcode.
        for probe in [
            Probe {
                video_codec: Some("h264".to_string()),
                ..base.clone()
            },
            Probe {
                audio_codec: Some("aac".to_string()),
                ..base.clone()
            },
            Probe {
                audio_codec: Some("mp3".to_string()),
                ..base.clone()
            },
            Probe {
                video_codec: None,
                ..base.clone()
            },
        ] {
            assert!(!probe_is_passthrough(&probe, 1024, DEFAULT_MAX_FILE_SIZE));
        }
        assert!(!probe_is_passthrough(&base, DEFAULT_MAX_FILE_SIZE + 1, DEFAULT_MAX_FILE_SIZE));
    }

    #[test]
    fn probe_parses_ffmpeg_i_stderr() {
        let stderr = "ffmpeg version 7.1 Copyright (c)\n\
             Input #0, matroska,webm, from 'in.webm':\n\
             Duration: 00:00:02.50, start: 0.000000, bitrate: 100 kb/s\n\
             Stream #0:0: Video: vp9 (Profile 0), yuv420p(tv, progressive), 640x480 [SAR 1:1 DAR 4:3], 25 fps\n\
             Stream #0:1(eng): Audio: opus, 48000 Hz, stereo, fltp\n";
        let probe = parse_probe_stderr(stderr);
        assert_eq!(probe.video_codec.as_deref(), Some("vp9"));
        assert_eq!(probe.audio_codec.as_deref(), Some("opus"));
        assert_eq!(probe.container.as_deref(), Some("matroska,webm"));
        assert!(probe.container_is_webm());
        assert_eq!((probe.width, probe.height), (Some(640), Some(480)));
        assert_eq!(probe.duration_ms, Some(2_500));
        // MP4 container: same codecs, not WebM.
        let mp4 = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'in.mp4':\n\
             Stream #0:0: Video: vp9 (Profile 0), yuv420p, 1280x720\n\
             Stream #0:1: Audio: opus, 48000 Hz, stereo\n";
        let probe = parse_probe_stderr(mp4);
        assert_eq!(probe.container.as_deref(), Some("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(!probe.container_is_webm());
        assert!(probe_is_passthrough(&probe, 1024, DEFAULT_MAX_FILE_SIZE));
        // Silent input: no audio stream, no duration.
        let silent = "  Stream #0:0: Video: h264, yuv420p, 1280x720\n";
        let probe = parse_probe_stderr(silent);
        assert_eq!(probe.video_codec.as_deref(), Some("h264"));
        assert_eq!(probe.audio_codec, None);
        assert_eq!(probe.container, None);
        assert!(!probe.container_is_webm());
        assert_eq!((probe.width, probe.height), (Some(1280), Some(720)));
        assert_eq!(probe.duration_ms, None);
        // Garbage parses to honest unknowns.
        assert_eq!(parse_probe_stderr(""), Probe::default());
    }

    #[test]
    fn remux_argv_copies_streams_without_reencode() {
        let config = TranscodeConfig::default();
        let argv = build_argv(
            std::path::Path::new("in.vid"),
            std::path::Path::new("out.webm"),
            TranscodePlan::VideoRemux,
            true,
            &config,
        );
        let joined = argv.join(" ");
        assert!(joined.contains("-map 0"), "{joined}");
        assert!(joined.contains("-c copy"), "{joined}");
        assert!(!joined.contains("libvpx"), "{joined}");
        assert!(!joined.contains("libopus"), "{joined}");
        assert!(joined.ends_with("out.webm"));
        assert_eq!(derived_extension(TranscodePlan::VideoRemux), "webm");
    }

    #[test]
    fn duration_parses_hh_mm_ss_ms() {
        assert_eq!(parse_duration_ms("00:00:02.50, start: 0"), Some(2_500));
        assert_eq!(parse_duration_ms("01:02:03.456, bitrate: 1"), Some(3_723_456));
        assert_eq!(parse_duration_ms("00:00:00.00,"), Some(0));
        assert_eq!(parse_duration_ms("N/A,"), None);
        assert_eq!(parse_duration_ms("garbage"), None);
    }

    #[test]
    fn argv_carries_metadata_and_quality_with_output_last() {
        let cfg = TranscodeConfig::default();
        let input = Path::new("/tmp/in.gif");
        let output = Path::new("/tmp/out.webm");
        for (plan, with_audio, video, audio) in [
            (TranscodePlan::ImageToWebp, false, "libwebp", ""),
            (TranscodePlan::GifToWebm, false, "libvpx-vp9", ""),
            (TranscodePlan::VideoToWebm, true, "libvpx-vp9", "libopus"),
            (TranscodePlan::VideoToWebm, false, "libvpx-vp9", ""),
        ] {
            let argv = build_argv(input, output, plan, with_audio, &cfg);
            assert!(argv.contains(&"-map_metadata".to_string()), "{plan:?}");
            assert!(argv.contains(&"0".to_string()), "{plan:?}");
            assert!(
                !argv.iter().any(|arg| arg == "-map_metadata -1"),
                "{plan:?}"
            );
            assert!(argv.contains(&video.to_string()), "{plan:?}");
            if !audio.is_empty() {
                assert!(argv.contains(&audio.to_string()), "{plan:?}");
            }
            // Fake-ffmpeg harness contract: `-i` precedes the input, the
            // output is the final argument.
            let i = argv.iter().position(|a| a == "-i").expect("has -i");
            assert_eq!(argv[i + 1], "/tmp/in.gif");
            assert_eq!(argv.last().map(String::as_str), Some("/tmp/out.webm"));
        }
        // Quality settings are the recorded Phase 7.1 defaults.
        let argv = build_argv(
            input,
            output,
            TranscodePlan::ImageToWebp,
            false,
            &cfg,
        );
        assert!(argv.contains(&"80".to_string()));
        let argv = build_argv(
            input,
            output,
            TranscodePlan::VideoToWebm,
            true,
            &cfg,
        );
        assert!(argv.contains(&"30".to_string()));
        assert!(argv.contains(&"96k".to_string()));
    }

    #[test]
    fn staging_key_is_a_deterministic_sibling() {
        assert_eq!(
            staging_key_for("aa/bb/cc/original.gif", "webm").as_deref(),
            Some("aa/bb/cc/derived.webm")
        );
        assert_eq!(staging_key_for("no-shard", "webm"), None);
        assert_eq!(staging_key_for("", "webm"), None);
    }

    #[test]
    fn missing_binary_is_a_startup_error() {
        let err = validate_toolchain("/nonexistent/birchtree-ffmpeg-xyz").expect_err("missing");
        assert!(err.contains("could not be executed"), "{err}");
        // Disabled transcoding validates without touching the binary.
        let disabled = TranscodeConfig {
            enabled: false,
            ffmpeg_path: "/nonexistent/birchtree-ffmpeg-xyz".to_string(),
            ..TranscodeConfig::default()
        };
        assert!(!disabled.validate().expect("disabled").enabled);
        // Nonsense knob values are startup errors too.
        let bad = TranscodeConfig {
            webp_quality: 101,
            ..TranscodeConfig::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn job_payload_round_trips() {
        let payload = TranscodeJobPayload { media_id: Id::new(42) };
        let json = payload.to_json();
        assert_eq!(TranscodeJobPayload::media_id_from_json(&json).unwrap(), Id::new(42));
        assert!(TranscodeJobPayload::media_id_from_json(&serde_json::json!({})).is_err());
    }

    #[test]
    fn default_config_validates_ranges_without_a_binary() {
        // Range checks run before the toolchain probe.
        let bad = TranscodeConfig {
            wall_time_secs: 0,
            ..config_for("ffmpeg")
        };
        assert!(bad.validate().is_err());
    }
}
