//! Media transcoding (Phase 7.1): ffmpeg-backed post-commit job.
//!
//! Decision table (decided 2026-10-17, `birchtree.md` §5 "Media transcoding"
//! and §7 "Phase 7.1"):
//!
//! - GIF (animated or not) → WebM with VP9, wire type `video`. `gifv` is
//!   never produced (deviations D1, D2).
//! - Video → WebM with VP9 and Opus audio (D3). Passthrough when the file is
//!   already VP9 video in a WebM container, with Opus audio or no audio, and
//!   within the upload size limit.
//! - Still images (JPEG, PNG, BMP, ...) → WebP (D4). WebP passes through.
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
//! Limits: wall-time, output-size and RSS caps per job ([`TranscodeLimits`]).
//! Quality settings (chosen at Phase 7.1 start, recorded as configuration
//! defaults): WebP `-quality 80`; VP9 constrained quality `-crf 30 -b:v 0`
//! with `-cpu-used 2 -row-mt 1`; Opus `-b:a 96k` with source channel layout.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::application::media::{THUMBNAIL_ERROR, UNSUPPORTED_REASON};
use crate::application::ports::{BlobStore, MediaProcessingResult, Store};
use crate::domain::{Error, Id, MediaState, Result};
use crate::media::processing;

// ---------------------------------------------------------------------------
// Configuration.
// ---------------------------------------------------------------------------

/// Default upload size cap: 16 MiB (Mastodon parity, mirrors
/// `Config::max_file_size` until the wiring patch threads it through).
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
    "png", "mjpeg", "gif", "bmp", "webp", "tiff", "h264", "hevc", "mpeg4", "vp8", "vp9",
    "aac", "opus", "vorbis", "mp3", "flac", "pcm_s16le",
];

/// Transcoding configuration (wiring patch: constructed from `Config`, see
/// the Phase 7.1 report's PATCHES FOR PARENT).
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
            return Err(format!("webp_quality {} out of range 0-100", self.webp_quality));
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

// ---------------------------------------------------------------------------
// Startup validation.
// ---------------------------------------------------------------------------

/// A working ffmpeg toolchain (probed at startup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainInfo {
    pub path: String,
    /// First line of `ffmpeg -version` (e.g. `ffmpeg version 7.1 ...`).
    pub version: String,
    /// False when transcoding is disabled (no binary was probed).
    pub enabled: bool,
}

impl ToolchainInfo {
    fn disabled() -> Self {
        Self {
            path: String::new(),
            version: String::new(),
            enabled: false,
        }
    }
}

/// Check that the ffmpeg binary runs and carries the VP9/Opus/WebP encoders
/// plus every decoder the accepted inputs (and the HEIC/HEIF/TIFF readiness
/// set) need. Any missing component is an `Err` listing it: startup must
/// fail rather than silently pass media through.
pub fn validate_toolchain(path: &str) -> std::result::Result<ToolchainInfo, String> {
    if path.trim().is_empty() {
        return Err("ffmpeg path must not be empty".to_string());
    }
    let version_out = Command::new(path)
        .arg("-hide_banner")
        .arg("-version")
        .output()
        .map_err(|err| format!("ffmpeg binary {path:?} could not be executed: {err}"))?;
    if !version_out.status.success() {
        return Err(format!(
            "ffmpeg binary {path:?} exited with status {}",
            version_out.status
        ));
    }
    let version = String::from_utf8_lossy(&version_out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(120)
        .collect::<String>();
    if !version.contains("ffmpeg version") {
        return Err(format!(
            "ffmpeg binary {path:?} did not report an ffmpeg version line"
        ));
    }
    let encoders = list_names(path, "-encoders")
        .map_err(|err| format!("ffmpeg {path:?} -encoders failed: {err}"))?;
    let mut missing: Vec<String> = REQUIRED_ENCODERS
        .iter()
        .filter(|name| !encoders.iter().any(|have| have.as_str() == **name))
        .map(|name| format!("encoder {name}"))
        .collect();
    let decoders = list_names(path, "-decoders")
        .map_err(|err| format!("ffmpeg {path:?} -decoders failed: {err}"))?;
    missing.extend(
        REQUIRED_DECODERS
            .iter()
            .filter(|name| !decoders.iter().any(|have| have.as_str() == **name))
            .map(|name| format!("decoder {name}")),
    );
    if !missing.is_empty() {
        return Err(format!(
            "ffmpeg binary {path:?} is missing required components: {}",
            missing.join(", ")
        ));
    }
    Ok(ToolchainInfo {
        path: path.to_string(),
        version,
        enabled: true,
    })
}

/// Parse an `ffmpeg -encoders` / `-decoders` listing into component names
/// (the second whitespace-separated field of each entry line).
fn list_names(path: &str, flag: &str) -> std::result::Result<Vec<String>, String> {
    let out = Command::new(path)
        .arg("-hide_banner")
        .arg(flag)
        .output()
        .map_err(|err| err.to_string())?;
    if !out.status.success() {
        return Err(format!("exit status {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?;
            fields.next().map(str::to_string)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Transcode decision.
// ---------------------------------------------------------------------------

/// What a stored upload needs. The video passthrough bound (already VP9
/// WebM, Opus-or-silent, within the size limit) is decided at job time by
/// probing the bytes ([`probe_is_passthrough`]); the static plan only says
/// the target shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodePlan {
    /// Already in target shape, or a class transcoding never touches
    /// (audio, unknown): keep the legacy processing path.
    Passthrough,
    /// Still image → WebP (wire type stays `image`).
    ImageToWebp,
    /// GIF (animated or not) → WebM/VP9 (wire type becomes `video`).
    GifToWebm,
    /// Video → WebM/VP9 + Opus (wire type stays `video`).
    VideoToWebm,
}

/// Static plan for a sniffed content type. Probe-dependent passthrough (VP9
/// WebM) still reports [`TranscodePlan::VideoToWebm`]; the job probes the
/// bytes before deciding.
pub fn plan_for_mime(mime_type: &str) -> TranscodePlan {
    match mime_type {
        "image/gif" => TranscodePlan::GifToWebm,
        "image/jpeg" | "image/png" | "image/bmp" => TranscodePlan::ImageToWebp,
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
        TranscodePlan::GifToWebm | TranscodePlan::VideoToWebm => "webm",
    }
}

// ---------------------------------------------------------------------------
// Stream probing (`ffmpeg -i` stderr parsing).
// ---------------------------------------------------------------------------

/// Stream facts parsed from `ffmpeg -i` stderr. `None` fields are honest
/// unknowns, never fabricated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// First video stream codec (`vp9`, `h264`, ...).
    pub video_codec: Option<String>,
    /// First audio stream codec (`opus`, `aac`, ...).
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}

/// VP9/WebM passthrough bound (D3): VP9 video, Opus-or-silent audio, within
/// the upload size limit. Positive identification only — an inconclusive
/// probe transcodes (never a silent passthrough of unknown bytes).
pub fn probe_is_passthrough(probe: &Probe, file_size: u64, max_file_size: u64) -> bool {
    file_size <= max_file_size
        && probe.video_codec.as_deref() == Some("vp9")
        && matches!(probe.audio_codec.as_deref(), None | Some("opus"))
}

/// Parse `ffmpeg -hide_banner -i <input>` stderr.
pub fn parse_probe_stderr(stderr: &str) -> Probe {
    let mut probe = Probe::default();
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Duration:") {
            probe.duration_ms = parse_duration_ms(rest);
        }
        if !line.contains("Stream #") {
            continue;
        }
        if line.contains("Video:") && probe.video_codec.is_none() {
            probe.video_codec = codec_after(line, "Video:");
            if probe.width.is_none() {
                let (width, height) = parse_dims_after(line, "Video:");
                probe.width = width;
                probe.height = height;
            }
        } else if line.contains("Audio:") && probe.audio_codec.is_none() {
            probe.audio_codec = codec_after(line, "Audio:");
        }
    }
    probe
}

/// Codec token after a `Video:`/`Audio:` marker (`vp9 (Profile 0), ...` →
/// `vp9`; lowercased).
fn codec_after(line: &str, marker: &str) -> Option<String> {
    let rest = line.split(marker).nth(1)?;
    let token = rest
        .trim()
        .split([',', ' ', '('])
        .next()?
        .trim()
        .to_lowercase();
    (!token.is_empty()).then_some(token)
}

/// First `WIDTHxHEIGHT` pair after the marker.
fn parse_dims_after(line: &str, marker: &str) -> (Option<u32>, Option<u32>) {
    let rest = line.split(marker).nth(1).unwrap_or("");
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'x' {
                if let Ok(width) = rest[start..i].parse::<u32>() {
                    let mut j = i + 1;
                    let height_start = j;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > height_start
                        && let Ok(height) = rest[height_start..j].parse::<u32>()
                        && width > 0
                        && height > 0
                    {
                        return (Some(width), Some(height));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    (None, None)
}

/// `HH:MM:SS.ms` (ffmpeg `Duration:` field) → milliseconds. `N/A` → `None`.
pub fn parse_duration_ms(field: &str) -> Option<u64> {
    let token = field.trim().split([',', ' ']).next()?.trim();
    if token == "N/A" {
        return None;
    }
    let (hms, ms) = match token.split_once('.') {
        Some((hms, ms)) => (hms, ms),
        None => (token, "0"),
    };
    let mut parts = hms.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let millis: u64 = format!("{ms:0<3}")
        .chars()
        .take(3)
        .collect::<String>()
        .parse()
        .ok()?;
    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis)
}

/// Probe a file with `<ffmpeg> -hide_banner -i <input>` and parse stderr.
/// The exit status is ignored (plain `-i` prints the facts either way). A
/// spawn failure is an `Err`, and the caller must treat it as "not provably
/// passthrough" → transcode. The input is always a local temp file, so the
/// probe cannot block on the network.
pub fn probe_file(ffmpeg_path: &str, input: &Path) -> std::result::Result<Probe, String> {
    let out = Command::new(ffmpeg_path)
        .arg("-hide_banner")
        .arg("-i")
        .arg(input)
        .output()
        .map_err(|err| format!("probe spawn failed: {err}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(parse_probe_stderr(&stderr))
}

// ---------------------------------------------------------------------------
// ffmpeg command construction.
// ---------------------------------------------------------------------------

/// Build the ffmpeg argv for a plan. Contract (relied on by tests and the
/// fake-ffmpeg harness): `-i` immediately precedes the input path, and the
/// output path is the final argument. Every command carries `-map_metadata
/// 0` and no strip/normalize filter: metadata (including EXIF and the
/// orientation tag) rides along where the target format holds it.
pub fn build_argv(
    ffmpeg_path: &str,
    input: &Path,
    output: &Path,
    plan: TranscodePlan,
    with_audio: bool,
    config: &TranscodeConfig,
) -> Vec<String> {
    let mut argv = vec![
        ffmpeg_path.to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        input.to_string_lossy().into_owned(),
        "-map_metadata".to_string(),
        "0".to_string(),
    ];
    match plan {
        TranscodePlan::ImageToWebp | TranscodePlan::Passthrough => {
            argv.extend([
                "-c:v".to_string(),
                "libwebp".to_string(),
                "-quality".to_string(),
                config.webp_quality.to_string(),
                "-compression_level".to_string(),
                "4".to_string(),
            ]);
        }
        TranscodePlan::GifToWebm => {
            argv.extend([
                "-an".to_string(),
                "-c:v".to_string(),
                "libvpx-vp9".to_string(),
                "-crf".to_string(),
                config.vp9_crf.to_string(),
                "-b:v".to_string(),
                "0".to_string(),
                "-pix_fmt".to_string(),
                // GIF transparency survives as WebM alpha.
                "yuva420p".to_string(),
                "-cpu-used".to_string(),
                config.vp9_cpu_used.to_string(),
                "-row-mt".to_string(),
                "1".to_string(),
            ]);
        }
        TranscodePlan::VideoToWebm => {
            argv.extend([
                "-map".to_string(),
                "0:v:0".to_string(),
                "-c:v".to_string(),
                "libvpx-vp9".to_string(),
                "-crf".to_string(),
                config.vp9_crf.to_string(),
                "-b:v".to_string(),
                "0".to_string(),
                "-pix_fmt".to_string(),
                "yuv420p".to_string(),
                "-cpu-used".to_string(),
                config.vp9_cpu_used.to_string(),
                "-row-mt".to_string(),
                "1".to_string(),
            ]);
            if with_audio {
                argv.extend([
                    "-map".to_string(),
                    "0:a:0".to_string(),
                    "-c:a".to_string(),
                    "libopus".to_string(),
                    "-b:a".to_string(),
                    format!("{}k", config.opus_bitrate_kbps),
                ]);
            } else {
                argv.push("-an".to_string());
            }
        }
    }
    argv.push(output.to_string_lossy().into_owned());
    argv
}

// ---------------------------------------------------------------------------
// Bounded ffmpeg execution.
// ---------------------------------------------------------------------------

/// How an ffmpeg run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeError {
    /// The binary could not be spawned (environmental → job retries).
    Spawn(String),
    /// I/O around temp files or pipes (environmental → job retries).
    Io(String),
    /// ffmpeg exited non-zero with deterministic input (terminal → `Failed`).
    FfmpegFailed { code: String, stderr_tail: String },
    /// Wall-time breach (terminal → `Failed`, per §7 Phase 7.1).
    Timeout { wall_ms: u64 },
    /// Output-size breach, during or after the run (terminal → `Failed`).
    OutputTooLarge { bytes: u64, limit: u64 },
    /// RSS cap breach (terminal → `Failed`).
    MemoryExceeded { bytes: u64, limit: u64 },
}

impl std::fmt::Display for TranscodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscodeError::Spawn(detail) => write!(f, "ffmpeg could not be spawned: {detail}"),
            TranscodeError::Io(detail) => write!(f, "transcode I/O failed: {detail}"),
            TranscodeError::FfmpegFailed { code, stderr_tail } => {
                write!(f, "ffmpeg exited {code}: {stderr_tail}")
            }
            TranscodeError::Timeout { wall_ms } => {
                write!(f, "transcode exceeded its wall-time ({wall_ms} ms)")
            }
            TranscodeError::OutputTooLarge { bytes, limit } => {
                write!(f, "transcode output {bytes} bytes exceeded the {limit}-byte limit")
            }
            TranscodeError::MemoryExceeded { bytes, limit } => {
                write!(f, "transcode RSS {bytes} bytes exceeded the {limit}-byte limit")
            }
        }
    }
}

impl std::error::Error for TranscodeError {}

/// Stats of a successful run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStats {
    pub output_bytes: u64,
    pub wall_ms: u64,
}

/// Keep the last `cap` bytes (ffmpeg diagnostics tail for failure reasons).
fn stderr_tail(stderr: &str, cap: usize) -> String {
    const ELLIPSIS: &str = "…";
    if stderr.len() <= cap {
        return stderr.to_string();
    }
    let cut = stderr.len() - cap;
    let mut start = cut;
    while start < stderr.len() && !stderr.is_char_boundary(start) {
        start += 1;
    }
    format!("{ELLIPSIS}{}", &stderr[start..])
}

/// Run `argv` with wall-time/output-size/RSS enforcement. `watch_path` is
/// the file ffmpeg writes (polled for the size cap, deleted on failure).
/// The output file must not exist on entry; on success it holds the derived
/// blob.
fn run_bounded(
    argv: &[String],
    watch_path: Option<&Path>,
    limits: &TranscodeLimits,
) -> std::result::Result<RunStats, TranscodeError> {
    if argv.is_empty() {
        return Err(TranscodeError::Spawn("empty argv".to_string()));
    }
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| TranscodeError::Spawn(err.to_string()))?;
    let pid = child.id();
    let start = Instant::now();
    let breach = loop {
        if let Some(_status) = child
            .try_wait()
            .map_err(|err| TranscodeError::Io(format!("wait for ffmpeg: {err}")))?
        {
            break None;
        }
        let wall_ms = start.elapsed().as_millis() as u64;
        if start.elapsed() > limits.wall_time {
            break Some(TranscodeError::Timeout { wall_ms });
        }
        if let Some(path) = watch_path
            && let Ok(meta) = std::fs::metadata(path)
            && meta.len() > limits.max_output_bytes
        {
            break Some(TranscodeError::OutputTooLarge {
                bytes: meta.len(),
                limit: limits.max_output_bytes,
            });
        }
        #[cfg(target_os = "linux")]
        if let Some(rss) = rss_bytes(pid)
            && rss > limits.max_memory_bytes
        {
            break Some(TranscodeError::MemoryExceeded {
                bytes: rss,
                limit: limits.max_memory_bytes,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(err) = breach {
        let _ = child.kill();
        let _ = child.wait();
        if let Some(path) = watch_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(err);
    }
    let wall_ms = start.elapsed().as_millis() as u64;
    // The child has exited; drain the (small: `-loglevel error`) stderr pipe.
    let mut stderr_bytes = Vec::new();
    if let Some(mut piped) = child.stderr.take()
        && let Err(err) = piped.read_to_end(&mut stderr_bytes)
    {
        return Err(TranscodeError::Io(format!("read ffmpeg stderr: {err}")));
    }
    let status = child
        .wait()
        .map_err(|err| TranscodeError::Io(format!("reap ffmpeg: {err}")))?;
    if !status.success() {
        if let Some(path) = watch_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(TranscodeError::FfmpegFailed {
            code: status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string()),
            stderr_tail: stderr_tail(&String::from_utf8_lossy(&stderr_bytes), 2048),
        });
    }
    if let Some(path) = watch_path {
        let len = std::fs::metadata(path)
            .map(|meta| meta.len())
            .map_err(|err| TranscodeError::Io(format!("stat transcode output: {err}")))?;
        if len == 0 {
            let _ = std::fs::remove_file(path);
            return Err(TranscodeError::FfmpegFailed {
                code: "0".to_string(),
                stderr_tail: "ffmpeg succeeded but wrote an empty output".to_string(),
            });
        }
        if len > limits.max_output_bytes {
            let _ = std::fs::remove_file(path);
            return Err(TranscodeError::OutputTooLarge {
                bytes: len,
                limit: limits.max_output_bytes,
            });
        }
        Ok(RunStats {
            output_bytes: len,
            wall_ms,
        })
    } else {
        Ok(RunStats {
            output_bytes: 0,
            wall_ms,
        })
    }
}

/// Current RSS of `pid` in bytes (`None` when unreadable). Linux only; other
/// platforms skip RSS enforcement (wall-time and output-size still apply).
#[cfg(target_os = "linux")]
fn rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Temp files and blob staging.
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh temp path that never collides within or across processes.
fn temp_path(prefix: &str, ext: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "birchtree-transcode-{prefix}-{}-{n}.{ext}",
        std::process::id()
    ))
}

/// Deletes its paths on drop (best-effort): a crash-safe temp-file guard.
struct TempGuard {
    paths: Vec<PathBuf>,
}

impl TempGuard {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Deterministic staging key for an attachment's in-flight derived blob:
/// the attachment shard with the `derived` style (`{dir}/derived.{ext}`).
/// The key is stable per attachment, so a crashed attempt's orphan is
/// overwritten by the retry, never accumulated; it is deleted after commit.
pub fn staging_key_for(file_key: &str, ext: &str) -> Option<String> {
    let (dir, _) = file_key.rsplit_once('/')?;
    if dir.is_empty() {
        return None;
    }
    Some(format!("{dir}/derived.{ext}"))
}

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
    let argv = build_argv(
        &config.ffmpeg_path,
        &input_path,
        &output_path,
        plan,
        with_audio,
        config,
    );
    let stats = run_bounded(&argv, Some(&output_path), &config.limits())?;
    let derived = std::fs::read(&output_path)
        .map_err(|err| TranscodeError::Io(format!("read transcode output: {err}")))?;
    Ok((derived, stats))
}

// ---------------------------------------------------------------------------
// Durable job payload and handler.
// ---------------------------------------------------------------------------

/// Payload of the post-commit transcode job (wiring patch: enqueued under a
/// new `JobKind::TranscodeMedia`; until that lands, the `ProcessMedia` hook
/// patch in the Phase 7.1 report routes through [`run_transcode_job`]).
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
                return Err(Error::Storage(format!("ffmpeg toolchain unavailable: {detail}")));
            }
            run_transcode_plan(store, blobs, &media, &bytes, plan, now_ms, config)
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
            let mut preview_key = None;
            let mut preview_width = None;
            let mut preview_height = None;
            if let Some(preview) = processed.preview {
                let key = sibling_key(&media.file_key, "small", preview.extension).ok_or_else(
                    || Error::Internal("media key carries no shard".into()),
                )?;
                blobs.put(&key, &preview.bytes)?;
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

/// Run the ffmpeg plan for `media` and commit the derived result.
fn run_transcode_plan(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media: &crate::domain::MediaAttachment,
    bytes: &[u8],
    plan: TranscodePlan,
    now_ms: u64,
    config: &TranscodeConfig,
) -> Result<()> {
    // Video probes the original first: an already-VP9/WebM/Opus-or-silent
    // file within the size limit passes through (D3).
    if plan == TranscodePlan::VideoToWebm
        && let Some(passthrough) = probe_passthrough(bytes, config)
        && passthrough
    {
        return commit_passthrough_video(store, media, bytes, now_ms, config);
    }
    let with_audio = plan == TranscodePlan::VideoToWebm && probe_has_audio(bytes, config);
    let input_ext = match plan {
        TranscodePlan::ImageToWebp => "img",
        TranscodePlan::GifToWebm => "gif",
        TranscodePlan::VideoToWebm => "vid",
        TranscodePlan::Passthrough => "bin",
    };
    match transcode_bytes(bytes, input_ext, plan, with_audio, config) {
        Ok((derived, _stats)) => commit_derived(store, blobs, media, bytes, &derived, plan, now_ms, config),
        Err(err) => match err {
            TranscodeError::Spawn(detail) | TranscodeError::Io(detail) => Err(Error::Storage(
                format!("transcode of media {} failed transiently: {detail}", media.id.value()),
            )),
            other => {
                tracing::warn!(media = media.id.value(), error = %other, "media transcode failed");
                mark_failed(store, media, &format!("transcode failed: {other}"), now_ms)?;
                Ok(())
            }
        },
    }
}

/// Probe `bytes` for the D3 passthrough bound. Any probe failure is "not
/// provably passthrough" → `None` → the job transcodes.
fn probe_passthrough(bytes: &[u8], config: &TranscodeConfig) -> Option<bool> {
    let input_path = temp_path("probe", "vid");
    let _guard = TempGuard::new(vec![input_path.clone()]);
    std::fs::write(&input_path, bytes).ok()?;
    let probe = probe_file(&config.ffmpeg_path, &input_path).ok()?;
    Some(probe_is_passthrough(&probe, bytes.len() as u64, config.max_file_size))
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
#[allow(clippy::too_many_arguments)]
fn commit_derived(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media: &crate::domain::MediaAttachment,
    original_bytes: &[u8],
    derived: &[u8],
    plan: TranscodePlan,
    now_ms: u64,
    config: &TranscodeConfig,
) -> Result<()> {
    // 1. Meta + (for stills) preview/blurhash come from the DERIVED bytes,
    //    never the original: dimensions must match the stored pixels.
    let (width, height, blurhash, preview, duration_ms) = match plan {
        TranscodePlan::ImageToWebp => match processing::process("image/webp", derived) {
            Ok(processed) => (
                processed.width,
                processed.height,
                processed.blurhash,
                processed.preview,
                None,
            ),
            Err(err) => {
                tracing::warn!(media = media.id.value(), error = %err, "transcoded image undecodable");
                mark_failed(
                    store,
                    media,
                    &format!("{}: ffmpeg output undecodable: {err}", UNSUPPORTED_REASON),
                    now_ms,
                )?;
                return Ok(());
            }
        },
        TranscodePlan::GifToWebm | TranscodePlan::VideoToWebm => {
            let (width, height, duration_ms) = probe_derived(derived, config);
            (width, height, None, None, duration_ms)
        }
        TranscodePlan::Passthrough => return Ok(()),
    };
    // 2. Stage the derived blob under its deterministic key and verify the
    //    read-back before it can ever be served.
    let staging_key = staging_key_for(&media.file_key, derived_extension(plan))
        .ok_or_else(|| Error::Internal("media key carries no shard".into()))?;
    blobs.put(&staging_key, derived)?;
    let staged = blobs.get(&staging_key)?.ok_or_else(|| {
        Error::Storage(format!("media {}: staged derived missing", media.id.value()))
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
        blobs.put(&key, &preview.bytes)?;
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
    // 6. Best-effort staging cleanup (a crash before this leaves a
    //    deterministic orphan the next run overwrites).
    if let Err(err) = blobs.delete(&staging_key) {
        tracing::warn!(media = media.id.value(), error = %err, "staged derived blob left in place");
    }
    let _ = original_bytes;
    Ok(())
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
fn mark_failed(
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
fn sibling_key(file_key: &str, style: &str, ext: &str) -> Option<String> {
    let (dir, _) = file_key.rsplit_once('/')?;
    if dir.is_empty() {
        return None;
    }
    Some(format!("{dir}/{style}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            width: Some(640),
            height: Some(480),
            duration_ms: Some(2_500),
        };
        assert!(probe_is_passthrough(&base, 1024, DEFAULT_MAX_FILE_SIZE));
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
        assert_eq!((probe.width, probe.height), (Some(640), Some(480)));
        assert_eq!(probe.duration_ms, Some(2_500));
        // Silent input: no audio stream, no duration.
        let silent = "  Stream #0:0: Video: h264, yuv420p, 1280x720\n";
        let probe = parse_probe_stderr(silent);
        assert_eq!(probe.video_codec.as_deref(), Some("h264"));
        assert_eq!(probe.audio_codec, None);
        assert_eq!((probe.width, probe.height), (Some(1280), Some(720)));
        assert_eq!(probe.duration_ms, None);
        // Garbage parses to honest unknowns.
        assert_eq!(parse_probe_stderr(""), Probe::default());
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
            let argv = build_argv("ffmpeg", input, output, plan, with_audio, &cfg);
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
            "ffmpeg",
            input,
            output,
            TranscodePlan::ImageToWebp,
            false,
            &cfg,
        );
        assert!(argv.contains(&"80".to_string()));
        let argv = build_argv(
            "ffmpeg",
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
