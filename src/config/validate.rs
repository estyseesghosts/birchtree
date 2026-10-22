//! Section validators for [`Config::validate`].
//!
//! Validate-only extraction (S9 slice group B). Each section checks one
//! configuration area with error strings copied verbatim from the original
//! inline `Config::validate` (`src/config.rs`, lines 410-626). The parent
//! owns the cutover: declare `mod validate;` in `src/config.rs` and delegate
//! `Config::validate` to [`validate`].
//!
//! Paths used (stable across the cutover): [`crate::config::Config`] and
//! [`crate::config::RemoteCacheMode`]. No other imports are needed; `url` is
//! referenced through its crate path.

use crate::config::{Config, RemoteCacheMode};

/// Runs every section check and returns the first error.
///
/// Section call order matches the original inline `Config::validate` field
/// order exactly, so first-error precedence for multi-invalid configs is
/// unchanged; within each section the original field order is preserved.
pub fn validate(config: &Config) -> Result<(), String> {
    validate_limits(config)?;
    validate_transcode(config)?;
    validate_feeds(config)?;
    validate_federation(config)?;
    validate_cache(config)?;
    validate_gaps(config)?;
    validate_relays(config)?;
    validate_site_files(config)?;
    validate_keyring(config)?;
    Ok(())
}

/// Scalar bounds, reset/age bounds, site-text paths and keyring exclusivity.
pub(crate) fn validate_limits(config: &Config) -> Result<(), String> {
    if config.public_url.trim().is_empty() {
        return Err("--public-url must not be empty".to_string());
    }
    if !(config.public_url.starts_with("http://") || config.public_url.starts_with("https://")) {
        return Err("--public-url must start with http:// or https://".to_string());
    }
    if config.data_dir.as_os_str().is_empty() {
        return Err("--data-dir must not be empty".to_string());
    }
    if config.max_file_size == 0 {
        return Err("max_file_size must be > 0".to_string());
    }
    if config.max_body_size == 0 {
        return Err("max_body_size must be > 0".to_string());
    }
    if config.max_file_size > config.max_body_size as u64 {
        return Err("max_file_size must not exceed max_body_size".to_string());
    }
    if config.max_status_chars == 0 {
        return Err("max_status_chars must be > 0".to_string());
    }
    if config.max_status_chars > 3000 {
        return Err("max_status_chars must not exceed 3000".to_string());
    }
    if config.worker_poll_interval.is_zero() {
        return Err("worker_poll_interval must be > 0".to_string());
    }
    Ok(())
}

/// Transcode toolchain numeric bounds (Phase 7.1).
pub(crate) fn validate_transcode(config: &Config) -> Result<(), String> {
    if config.transcode_wall_time_secs == 0 {
        return Err("transcode_wall_time_secs must be > 0".to_string());
    }
    if config.transcode_max_output_bytes == 0 {
        return Err("transcode_max_output_bytes must be > 0".to_string());
    }
    if config.transcode_max_memory_bytes == 0 {
        return Err("transcode_max_memory_bytes must be > 0".to_string());
    }
    if config.webp_quality > 100 {
        return Err(format!(
            "webp_quality {} out of range 0-100",
            config.webp_quality
        ));
    }
    if config.vp9_crf > 63 {
        return Err(format!("vp9_crf {} out of range 0-63", config.vp9_crf));
    }
    if config.vp9_cpu_used > 5 {
        return Err(format!(
            "vp9_cpu_used {} out of range 0-5",
            config.vp9_cpu_used
        ));
    }
    if config.opus_bitrate_kbps == 0 {
        return Err("opus_bitrate_kbps must be > 0".to_string());
    }
    Ok(())
}

/// Timeline feed gates and core-extension policy enums.
pub(crate) fn validate_feeds(config: &Config) -> Result<(), String> {
    for (key, value) in [
        ("local_live_feed_access", &config.local_live_feed_access),
        ("remote_live_feed_access", &config.remote_live_feed_access),
        ("remote_topic_feed_access", &config.remote_topic_feed_access),
    ] {
        if !["public", "authenticated", "disabled"].contains(&value.as_str()) {
            return Err(format!(
                "{key} must be one of public, authenticated, disabled"
            ));
        }
    }
    if !["public", "authenticated"].contains(&config.local_topic_feed_access.as_str()) {
        return Err("local_topic_feed_access must be one of public, authenticated".to_string());
    }
    if !["off", "unenforced", "enforced"].contains(&config.license_policy.as_str()) {
        return Err("license_policy must be one of off, unenforced, enforced".to_string());
    }
    if !["off", "unenforced", "enforced"].contains(&config.place_policy.as_str()) {
        return Err("place_policy must be one of off, unenforced, enforced".to_string());
    }
    if !["off", "nag", "block"].contains(&config.alt_text_policy.as_str()) {
        return Err("alt_text_policy must be one of off, nag, block".to_string());
    }
    Ok(())
}

/// Outbound endpoints: streaming base URL, mail provider and relay inboxes.
pub(crate) fn validate_federation(config: &Config) -> Result<(), String> {
    if let Some(base) = &config.streaming_api_base_url {
        let valid = ["ws://", "wss://", "http://", "https://"]
            .iter()
            .any(|scheme| base.starts_with(scheme) && base.len() > scheme.len());
        if !valid {
            return Err(
                "--streaming-url must start with ws://, wss://, http:// or https://".to_string(),
            );
        }
    }
    match (&config.smtp_url, &config.mail_from) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(
                "--mail-from is set but --smtp-url is not; mail would never send".to_string(),
            );
        }
        (Some(url), from) => {
            let rest = url
                .strip_prefix("smtp://")
                .or_else(|| url.strip_prefix("smtps://"));
            if rest.is_none_or(|r| r.is_empty()) {
                return Err("--smtp-url must start with smtp:// or smtps://".to_string());
            }
            match from {
                Some(addr) if addr.contains('@') => {}
                _ => {
                    return Err(
                        "--smtp-url is set but --mail-from is missing or invalid; refusal address required"
                            .to_string(),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Gaps batch reset-bearer lifetime + minimum age (runs after the cache
/// checks, matching the original inline order).
pub(crate) fn validate_gaps(config: &Config) -> Result<(), String> {
    if config.password_reset_ttl_hours == 0 || config.password_reset_ttl_hours > 72 {
        return Err("password_reset_ttl_hours must be between 1 and 72".to_string());
    }
    if config.minimum_age_years < 13 || config.minimum_age_years > 21 {
        return Err("minimum_age_years must be between 13 and 21".to_string());
    }
    Ok(())
}

/// Site text file paths must be non-empty when set (existence is checked at
/// serve time, not here).
pub(crate) fn validate_site_files(config: &Config) -> Result<(), String> {
    for (key, path) in [
        ("site_terms_file", &config.site_terms_file),
        ("site_privacy_file", &config.site_privacy_file),
        ("site_extended_file", &config.site_extended_file),
    ] {
        if path.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
            return Err(format!("{key} must not be empty"));
        }
    }
    Ok(())
}

/// Keyring source exclusivity and non-emptiness.
pub(crate) fn validate_keyring(config: &Config) -> Result<(), String> {
    if config.secret_keyring_file.is_some() && config.secret_keyring_env.is_some() {
        return Err(
            "secret_keyring_file and secret_keyring_env are mutually exclusive".to_string(),
        );
    }
    if config
        .secret_keyring_file
        .as_ref()
        .is_some_and(|p| p.as_os_str().is_empty())
    {
        return Err("secret_keyring_file must not be empty".to_string());
    }
    if config
        .secret_keyring_env
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err("secret_keyring_env must not be empty".to_string());
    }
    Ok(())
}

/// Relay inboxes URL shape (runs after the site-file checks, matching the
/// original inline order).
pub(crate) fn validate_relays(config: &Config) -> Result<(), String> {
    for relay in &config.relay_inboxes {
        let parsed = url::Url::parse(relay)
            .map_err(|_| "relay_inboxes entries must be http(s) inbox URLs".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err("relay_inboxes entries must be http(s) inbox URLs".to_string());
        }
    }
    Ok(())
}

/// Remote-media cache budgets and the cache/durable directory split.
pub(crate) fn validate_cache(config: &Config) -> Result<(), String> {
    if config.remote_cache.mode == RemoteCacheMode::Disabled {
        return Err(
            "remote_cache.mode = disabled is not supported yet: no non-persisting delivery path"
                .to_string(),
        );
    }
    if config.remote_cache.global_cap_bytes == 0 {
        return Err("remote_cache.global_cap_bytes must be > 0".to_string());
    }
    if config.remote_cache.avatar_emoji_cap_bytes == 0 {
        return Err("remote_cache.avatar_emoji_cap_bytes must be > 0".to_string());
    }
    if config.remote_cache.preview_cap_bytes == 0 {
        return Err("remote_cache.preview_cap_bytes must be > 0".to_string());
    }
    if config.remote_cache.full_cap_bytes == 0 {
        return Err("remote_cache.full_cap_bytes must be > 0".to_string());
    }
    if config.remote_cache.avatar_retention_days == 0 {
        return Err("remote_cache.avatar_retention_days must be > 0".to_string());
    }
    let reserved = config
        .remote_cache
        .avatar_emoji_cap_bytes
        .saturating_add(config.remote_cache.preview_cap_bytes)
        .saturating_add(config.remote_cache.full_cap_bytes);
    if config.remote_cache.global_cap_bytes < reserved {
        return Err("remote_cache.global_cap_bytes must cover the reserved class caps".to_string());
    }
    if config.cache_dir_resolved() == config.durable_dir_resolved() {
        return Err("cache_dir and durable_dir must be different directories".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    use super::validate;

    fn error_for(mutate: impl FnOnce(&mut Config)) -> String {
        let mut config = Config::default();
        mutate(&mut config);
        validate(&config).expect_err("invalid config must fail validation")
    }

    #[test]
    fn default_config_passes_section_validators() {
        assert!(validate(&Config::default()).is_ok());
    }

    #[test]
    fn rejects_non_http_public_url_with_exact_string() {
        let err = error_for(|config| config.public_url = "ftp://example.social".to_string());
        assert_eq!(err, "--public-url must start with http:// or https://");
    }

    #[test]
    fn rejects_file_size_above_body_size_with_exact_string() {
        let err = error_for(|config| config.max_file_size = config.max_body_size as u64 + 1);
        assert_eq!(err, "max_file_size must not exceed max_body_size");
    }

    #[test]
    fn rejects_webp_quality_above_100_with_exact_string() {
        let err = error_for(|config| config.webp_quality = 101);
        assert_eq!(err, "webp_quality 101 out of range 0-100");
    }

    #[test]
    fn rejects_vp9_crf_above_63_with_exact_string() {
        let err = error_for(|config| config.vp9_crf = 64);
        assert_eq!(err, "vp9_crf 64 out of range 0-63");
    }

    #[test]
    fn rejects_unknown_live_feed_mode_with_exact_string() {
        let err = error_for(|config| config.local_live_feed_access = "everyone".to_string());
        assert_eq!(
            err,
            "local_live_feed_access must be one of public, authenticated, disabled"
        );
    }

    #[test]
    fn rejects_unknown_topic_feed_mode_with_exact_string() {
        let err = error_for(|config| config.remote_topic_feed_access = "everyone".to_string());
        assert_eq!(
            err,
            "remote_topic_feed_access must be one of public, authenticated, disabled"
        );
    }

    #[test]
    fn rejects_disabled_local_topic_feed_with_exact_string() {
        let err = error_for(|config| config.local_topic_feed_access = "disabled".to_string());
        assert_eq!(
            err,
            "local_topic_feed_access must be one of public, authenticated"
        );
    }
}
