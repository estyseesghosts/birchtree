//! Toolchain startup validation (S12 split of `super`).

use std::process::Command;

use super::config::{REQUIRED_DECODERS, REQUIRED_ENCODERS};

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
    pub(super) fn disabled() -> Self {
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
