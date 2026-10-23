//! ffmpeg argv construction (S12 split of `super`).

use std::path::Path;

use super::config::TranscodeConfig;
use super::plan::TranscodePlan;

// ---------------------------------------------------------------------------
// ffmpeg command construction.
// ---------------------------------------------------------------------------

/// Build the ffmpeg argv for a plan. Contract (relied on by tests and the
/// fake-ffmpeg harness): `-i` immediately precedes the input path, and the
/// output path is the final argument. Every command carries `-map_metadata
/// 0` and no strip/normalize filter: metadata (including EXIF and the
/// orientation tag) rides along where the target format holds it.
pub fn build_argv(
    input: &Path,
    output: &Path,
    plan: TranscodePlan,
    with_audio: bool,
    config: &TranscodeConfig,
) -> Vec<String> {
    let mut argv = vec![
        config.ffmpeg_path.clone(),
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
        TranscodePlan::VideoRemux => {
            // Stream copy into the output container (`.webm` via
            // `derived_extension`): no re-encode, no quality loss, metadata
            // rides along with `-map_metadata 0`. `with_audio` is ignored —
            // every stream is copied.
            argv.extend([
                "-map".to_string(),
                "0".to_string(),
                "-c".to_string(),
                "copy".to_string(),
            ]);
        }
    }
    argv.push(output.to_string_lossy().into_owned());
    argv
}
