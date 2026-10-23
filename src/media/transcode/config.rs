//! Transcode configuration and per-job limits (S12 split of `super`).

use std::time::Duration;

use super::toolchain::{ToolchainInfo, validate_toolchain};

// ---------------------------------------------------------------------------
// Configuration.
// ---------------------------------------------------------------------------

/// Default upload size cap: 16 MiB (Mastodon parity, mirrors
/// `Config::max_file_size`, which [`TranscodeConfig::max_file_size`] copies at
/// startup via `Config::transcode_config`).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;

/// Default per-job derived-output cap: 64 MiB.
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Default per-job wall-time: 120 s.
pub const DEFAULT_WALL_TIME_SECS: u64 = 120;

/// Default per-job resident-memory cap: 1 GiB.
pub const DEFAULT_MAX_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;

/// Probe timeout is inherited from the file under test: `probe_file` reads a
/// local temp file, so it cannot block on the network; a corrupt file fails
/// fast. Callers treat any probe failure as "not provably passthrough" and
/// transcode.

/// Encoders the startup check requires (`ffmpeg -encoders` names).
pub const REQUIRED_ENCODERS: &[&str] = &["libvpx-vp9", "libopus", "libwebp"];

/// Decoders the startup check requires (`ffmpeg -decoders` names): every
/// accepted still/video/audio input, plus TIFF and HEVC (HEIC/HEIF
/// readiness — the sniffer does not accept those yet, see the Phase 7.1
/// report).
pub const REQUIRED_DECODERS: &[&str] = &[
    "png",
    "mjpeg",
    "gif",
    "bmp",
    "webp",
    "tiff",
    "h264",
    "hevc",
    "mpeg4",
    "vp8",
    "vp9",
    "aac",
    "opus",
    "vorbis",
    "mp3",
    "flac",
    "pcm_s16le",
];

/// Transcoding configuration, built from [`crate::config::Config`] at
/// startup (see `Config::transcode_config`): CLI/config knobs for quality
/// (`webp_quality`, `vp9_crf`, `vp9_cpu_used`, `opus_bitrate_kbps`), caps
/// (`max_file_size`, `max_output_bytes`, `max_memory_bytes`, wall time) and
/// the toolchain path.
#[derive(Debug, Clone)]
pub struct TranscodeConfig {
    /// Master switch. `validate` enforces the toolchain only when set; the
    /// upload-path hook routes around this module entirely when unset.
    pub enabled: bool,
    /// ffmpeg binary (absolute path or `PATH` lookup, e.g. `"ffmpeg"`).
    pub ffmpeg_path: String,
    /// libwebp `-quality` (0–100).
    pub webp_quality: u8,
    /// VP9 `-crf` (0–63, constrained-quality mode with `-b:v 0`).
    pub vp9_crf: u8,
    /// VP9 `-cpu-used` (0–5; higher is faster and slightly worse).
    pub vp9_cpu_used: u8,
    /// Opus `-b:a` in kbit/s.
    pub opus_bitrate_kbps: u32,
    /// Per-job wall-time in seconds.
    pub wall_time_secs: u64,
    /// Per-job derived-output cap in bytes.
    pub max_output_bytes: u64,
    /// Per-job resident-memory (RSS) cap in bytes (enforced by polling
    /// `/proc/<pid>/status` on Linux; best-effort elsewhere).
    pub max_memory_bytes: u64,
    /// Upload size cap in bytes (VP9/WebM passthrough bound).
    pub max_file_size: u64,
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ffmpeg_path: "ffmpeg".to_string(),
            webp_quality: 80,
            vp9_crf: 30,
            vp9_cpu_used: 2,
            opus_bitrate_kbps: 96,
            wall_time_secs: DEFAULT_WALL_TIME_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
        }
    }
}

impl TranscodeConfig {
    /// Per-job enforcement caps.
    pub fn limits(&self) -> TranscodeLimits {
        TranscodeLimits {
            wall_time: Duration::from_secs(self.wall_time_secs.max(1)),
            max_output_bytes: self.max_output_bytes,
            max_memory_bytes: self.max_memory_bytes,
        }
    }

    /// Validate the configuration. When transcoding is enabled this runs the
    /// toolchain check ([`validate_toolchain`]): a missing or non-working
    /// ffmpeg is a startup error, never a silent passthrough. Returns the
    /// probed toolchain on success.
    pub fn validate(&self) -> std::result::Result<ToolchainInfo, String> {
        if self.webp_quality > 100 {
            return Err(format!(
                "webp_quality {} out of range 0-100",
                self.webp_quality
            ));
        }
        if self.vp9_crf > 63 {
            return Err(format!("vp9_crf {} out of range 0-63", self.vp9_crf));
        }
        if self.vp9_cpu_used > 5 {
            return Err(format!(
                "vp9_cpu_used {} out of range 0-5",
                self.vp9_cpu_used
            ));
        }
        if self.opus_bitrate_kbps == 0 {
            return Err("opus_bitrate_kbps must be > 0".to_string());
        }
        if self.wall_time_secs == 0 {
            return Err("transcode wall-time must be > 0".to_string());
        }
        if self.max_output_bytes == 0 {
            return Err("transcode max_output_bytes must be > 0".to_string());
        }
        if self.max_memory_bytes == 0 {
            return Err("transcode max_memory_bytes must be > 0".to_string());
        }
        if self.max_file_size == 0 {
            return Err("max_file_size must be > 0".to_string());
        }
        if !self.enabled {
            return Ok(ToolchainInfo::disabled());
        }
        validate_toolchain(&self.ffmpeg_path)
    }
}

/// Per-job enforcement caps.
#[derive(Debug, Clone, Copy)]
pub struct TranscodeLimits {
    pub wall_time: Duration,
    pub max_output_bytes: u64,
    pub max_memory_bytes: u64,
}
