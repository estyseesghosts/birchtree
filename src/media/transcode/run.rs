//! Bounded ffmpeg execution (S12 split of `super`).

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::media::upload_gate;

use super::config::TranscodeLimits;

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
                write!(
                    f,
                    "transcode output {bytes} bytes exceeded the {limit}-byte limit"
                )
            }
            TranscodeError::MemoryExceeded { bytes, limit } => {
                write!(
                    f,
                    "transcode RSS {bytes} bytes exceeded the {limit}-byte limit"
                )
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
pub(super) fn run_bounded(
    argv: &[String],
    watch_path: Option<&Path>,
    limits: &TranscodeLimits,
) -> std::result::Result<RunStats, TranscodeError> {
    if argv.is_empty() {
        return Err(TranscodeError::Spawn("empty argv".to_string()));
    }
    // At most `ffmpeg_gate()` FFmpeg processes run at once; extra jobs wait
    // here (on a blocking-pool thread) instead of oversubscribing the host.
    let _slot = upload_gate::ffmpeg_gate()
        .acquire()
        .map_err(|err| TranscodeError::Io(format!("ffmpeg slot unavailable: {err}")))?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| TranscodeError::Spawn(err.to_string()))?;
    let pid = child.id();
    let start = Instant::now();
    let breach = poll_until_exit(&mut child, watch_path, limits, start, pid)?;
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
            code: status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
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

/// Poll a running ffmpeg child until it exits or a limit breaches
/// (`run_bounded` stage, verbatim): wall-time, output-size and (Linux) RSS
/// enforcement. Returns the breach, if any; the caller kills and drains.
fn poll_until_exit(
    child: &mut std::process::Child,
    watch_path: Option<&Path>,
    limits: &TranscodeLimits,
    start: std::time::Instant,
    _pid: u32,
) -> std::result::Result<Option<TranscodeError>, TranscodeError> {
    loop {
        if let Some(_status) = child
            .try_wait()
            .map_err(|err| TranscodeError::Io(format!("wait for ffmpeg: {err}")))?
        {
            break Ok(None);
        }
        let wall_ms = start.elapsed().as_millis() as u64;
        if start.elapsed() > limits.wall_time {
            break Ok(Some(TranscodeError::Timeout { wall_ms }));
        }
        if let Some(path) = watch_path
            && let Ok(meta) = std::fs::metadata(path)
            && meta.len() > limits.max_output_bytes
        {
            break Ok(Some(TranscodeError::OutputTooLarge {
                bytes: meta.len(),
                limit: limits.max_output_bytes,
            }));
        }
        #[cfg(target_os = "linux")]
        if let Some(rss) = rss_bytes(_pid)
            && rss > limits.max_memory_bytes
        {
            break Ok(Some(TranscodeError::MemoryExceeded {
                bytes: rss,
                limit: limits.max_memory_bytes,
            }));
        }
        std::thread::sleep(Duration::from_millis(50));
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
