//! Stream probing via `ffmpeg -i` stderr parsing (S12 split of `super`).

use std::path::Path;
use std::process::Command;

use crate::media::upload_gate;

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
    /// Container format list from the `Input #0, ...` line
    /// (`matroska,webm`, `mov,mp4,m4a,3gp,3g2,mj2`, ...).
    pub container: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}

impl Probe {
    /// True when the probed container is WebM (`Input #0` format list
    /// contains `webm`). An unknown container is never WebM.
    pub fn container_is_webm(&self) -> bool {
        self.container
            .as_deref()
            .is_some_and(|formats| formats.split(',').any(|f| f.trim() == "webm"))
    }
}

/// Codec/size bound for video passthrough (D3): VP9 video, Opus-or-silent
/// audio, within the upload size limit. Positive identification only — an
/// inconclusive probe transcodes (never a silent passthrough of unknown
/// bytes). The container is decided separately: WebM keeps its bytes (see
/// [`Probe::container_is_webm`]), any other container is remuxed with
/// stream-copy into WebM (no re-encode). Like upstream Mastodon's
/// `eligible_to_passthrough?` (codec-based, no container check), with a
/// cheap remux on top so served bytes are always WebM.
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
        if let Some(rest) = line.strip_prefix("Input #") {
            // `Input #0, matroska,webm, from 'in.webm':` → `matroska,webm`.
            if probe.container.is_none()
                && let Some(formats) = rest.split_once(',').map(|(_, tail)| tail)
                && let Some(formats) = formats.rsplit_once(", from").map(|(head, _)| head)
            {
                let formats = formats.trim();
                if !formats.is_empty() {
                    probe.container = Some(formats.to_lowercase());
                }
            }
            continue;
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
    // Probes are FFmpeg processes too: they share the concurrency cap.
    let _slot = upload_gate::ffmpeg_gate()
        .acquire()
        .map_err(|err| format!("probe slot unavailable: {err}"))?;
    let out = Command::new(ffmpeg_path)
        .arg("-hide_banner")
        .arg("-i")
        .arg(input)
        .output()
        .map_err(|err| format!("probe spawn failed: {err}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(parse_probe_stderr(&stderr))
}
