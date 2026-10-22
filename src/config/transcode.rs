//! Transcode pipeline configuration (S9 section of `crate::config`).
//!
//! Verbatim move of `Config::transcode_config` (`config.rs:760-781`), the
//! `TranscodeConfig` reference built from the transcode knobs
//! (`transcoding_enabled`, `ffmpeg_path`, `webp_quality`, `vp9_crf`,
//! `vp9_cpu_used`, `opus_bitrate_kbps`, `transcode_wall_time_secs`,
//! `transcode_max_output_bytes`, `transcode_max_memory_bytes`;
//! declarations `config.rs:224-244`, defaults `config.rs:370-378`). The knob
//! fields stay on `Config`; the parent cutover deletes the original method
//! and wires this module. No behavior change; error strings are
//! byte-identical.

impl crate::config::Config {
    /// Build the Phase 7.1 transcode configuration, proving the toolchain
    /// when transcoding is enabled: a missing or non-working ffmpeg is an
    /// error here (serve startup), never a silent passthrough. CLI and
    /// harness paths never call this, so they never need ffmpeg.
    pub fn transcode_config(&self) -> Result<crate::media::transcode::TranscodeConfig, String> {
        let config = crate::media::transcode::TranscodeConfig {
            enabled: self.transcoding_enabled,
            ffmpeg_path: self.ffmpeg_path.clone(),
            webp_quality: self.webp_quality,
            vp9_crf: self.vp9_crf,
            vp9_cpu_used: self.vp9_cpu_used,
            opus_bitrate_kbps: self.opus_bitrate_kbps,
            wall_time_secs: self.transcode_wall_time_secs,
            max_output_bytes: self.transcode_max_output_bytes,
            max_memory_bytes: self.transcode_max_memory_bytes,
            max_file_size: self.max_file_size,
        };
        if self.transcoding_enabled {
            config.validate().map_err(|e| format!("transcoding: {e}"))?;
        }
        Ok(config)
    }
}
