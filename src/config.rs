use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

/// Millis-since-epoch style `Duration` encoding for the JSON config file:
/// a plain number of milliseconds (`worker_poll_interval_ms`).
mod serde_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u64(value.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(de)?))
    }
}

/// Remote-media cache backing mode (`docs/remote-media-cache-plan.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCacheMode {
    /// No persistence; direct/streaming proxy delivery (no proxy design yet).
    Disabled,
    /// Fetch on first request, cache, auto-evict (default).
    OnDemand,
    /// Proactive fetch behind config + rate limit + budget (scoped warming).
    Warm,
}

impl Default for RemoteCacheMode {
    fn default() -> Self {
        Self::OnDemand
    }
}

/// Byte budgets + lifecycle for the remote-media cache (Step 0 skeleton:
/// config surface only; enforcement lands with agents B/C).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteCacheConfig {
    pub mode: RemoteCacheMode,
    /// Hard global byte cap (all classes + in-flight).
    pub global_cap_bytes: u64,
    /// Reserved class: avatars, headers, custom emoji.
    pub avatar_emoji_cap_bytes: u64,
    /// Reserved class: image previews / thumbnails.
    pub preview_cap_bytes: u64,
    /// Reserved class: full image originals.
    pub full_cap_bytes: u64,
    /// Avatar/emoji entry retention in days.
    pub avatar_retention_days: u64,
    /// Scoped warming reuse of `FetchRemoteMedia` (off by default,
    /// relay-excluded when enabled).
    pub warming: bool,
}

impl Default for RemoteCacheConfig {
    fn default() -> Self {
        Self {
            mode: RemoteCacheMode::OnDemand,
            global_cap_bytes: 512 * 1024 * 1024,
            avatar_emoji_cap_bytes: 128 * 1024 * 1024,
            preview_cap_bytes: 256 * 1024 * 1024,
            full_cap_bytes: 128 * 1024 * 1024,
            avatar_retention_days: 30,
            warming: false,
        }
    }
}

/// Server configuration. Validated before any services are built.
///
/// JSON keys match the field names, except `worker_poll_interval_ms`
/// (milliseconds). Every key is optional in the file: absent keys take the
/// [`Default`] values below. Unknown keys are rejected (typo safety).
/// CLI flags override file values; `validate()` runs once over the merged
/// result.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Public base URL used for actor/object URIs (must be http/https).
    pub public_url: String,
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// Addresses trusted to set forwarding headers (`X-Forwarded-For`).
    pub trusted_proxies: Vec<IpAddr>,
    /// Maximum media upload size in bytes.
    pub max_file_size: u64,
    /// Maximum request body size in bytes for the HTTP layer.
    pub max_body_size: usize,
    /// Maximum status text length in characters (Mastodon-compatible count:
    /// URLs count as 23). Default 500 (Mastodon parity); configurable up to
    /// 3000 (Misskey parity) via `--max-status-chars=`.
    pub max_status_chars: usize,
    /// Worker poll interval when the queue is empty. JSON key
    /// `worker_poll_interval_ms` (milliseconds).
    #[serde(rename = "worker_poll_interval_ms", with = "serde_millis")]
    pub worker_poll_interval: Duration,
    /// Whether new account registration is open. Phase 1 keeps this closed;
    /// the CLI bootstrap is always available.
    pub registration_open: bool,
    /// Whether public registration creates accounts awaiting approval
    /// (upstream `Setting.registrations_mode == "approved"`). New accounts
    /// land with `approved = false`, appear under the admin `pending`
    /// filter, and their tokens are refused by user-token endpoints until
    /// an admin approves them. Off by default (upstream default `open`).
    pub registration_requires_approval: bool,
    /// Instance title shown by `/api/v1/instance` and `/api/v2/instance`.
    pub site_title: String,
    /// Instance description.
    pub site_description: String,
    /// Contact address advertised by the instance endpoints.
    pub site_contact_email: String,
    /// Contact account handle (`username`, local), if configured.
    pub site_contact_username: Option<String>,
    /// Streaming API base URL (`ws`, `wss`, `http` or `https`), advertised as
    /// `urls.streaming`. Unset: streams are served from the public host and
    /// advertised as its `ws`/`wss` form. Set to another host: requests to the
    /// streaming paths on this host answer `301` to that host (upstream's
    /// `StreamingController`).
    pub streaming_api_base_url: Option<String>,
    /// Allow `http://` federation targets. Off by default: production is HTTPS only.
    /// Set only for a lab network (`--federation-allow-http`).
    pub federation_allow_http: bool,
    /// Private ranges that federation may reach, CIDR list. Empty by default.
    /// Set only for a lab network (`--federation-allow-private=CIDR,...`), as
    /// Mastodon's `ALLOWED_PRIVATE_ADDRESSES` does.
    pub federation_allowlist: Vec<crate::federation::policy::Cidr>,
    /// PEM file of extra trust roots for federation TLS. Set only for a lab
    /// certificate authority (`--federation-extra-ca=PATH`); empty by default.
    pub federation_extra_ca: Option<std::path::PathBuf>,
    /// Authorized fetch (Mastodon's `AUTHORIZED_FETCH`): the note, quote-authorization
    /// and collection reads need a valid HTTP signature. Off by default.
    pub federation_authorized_fetch: bool,
    /// Multiplier on the API rate limit (1 = production limit). Raised only for
    /// load tests on a debug node (`--debug-weak-rate-limits` or
    /// `--rate-limit-multiplier=N`).
    pub rate_limit_multiplier: u32,
    /// Whether the public trends endpoints serve approved items (upstream
    /// `Setting.trends`). On by default; when false the trends reads return
    /// empty arrays. (No CLI flag yet; operators stop the server to change it
    /// via configuration in a later phase.)
    pub trends_enabled: bool,
    /// Search backend switch (Phase 7.2): `embedded` (default, in-process
    /// index), `database` (bounded local scan), or `off` (no status hits,
    /// matching upstream with no engine).
    pub search_mode: crate::search::SearchMode,
    /// Whether the ffmpeg transcode pipeline runs (Phase 7.1). On by
    /// default; `--no-transcode` disables it (uploads keep the Phase 4
    /// passthrough behavior).
    pub transcoding_enabled: bool,    /// ffmpeg binary path (Phase 7.1).
    pub ffmpeg_path: String,
    /// libwebp `-quality` 0–100 (Phase 7.1; still-image pipeline).
    pub webp_quality: u8,
    /// VP9 `-crf` 0–63, constrained-quality mode with `-b:v 0` (Phase 7.1;
    /// GIF and video pipelines).
    pub vp9_crf: u8,
    /// VP9 `-cpu-used` 0–5; higher is faster and slightly worse (Phase 7.1).
    pub vp9_cpu_used: u8,
    /// Opus `-b:a` in kbit/s (Phase 7.1; video-with-audio pipeline).
    pub opus_bitrate_kbps: u32,
    /// Per-job wall-time limit in seconds (Phase 7.1).
    pub transcode_wall_time_secs: u64,
    /// Per-job output-size limit in bytes (Phase 7.1).
    pub transcode_max_output_bytes: u64,
    /// Per-job memory limit in bytes (Phase 7.1).
    pub transcode_max_memory_bytes: u64,
    /// VAPID public key (base64url, 65-byte point) advertised by push
    /// subscription serializers. Empty by default (delivery jobs
    /// retry-then-quarantine until keys are configured).
    pub vapid_public_key: String,
    /// VAPID private key (base64url, 32-byte scalar) used to sign delivery
    /// requests. Empty by default; never serialized.
    pub vapid_private_key: String,
    /// Directory holding keystodon's built first-party bundle. `None`
    /// (default) means the bundle is not deployed: `/app/*` answers 404.
    pub web_client_dir: Option<PathBuf>,
    /// Whether invite tokens are honored (round 3, area A). When false, a
    /// supplied `invite_code` is refused with reason and invites never open
    /// closed registrations. On by default (upstream always models invites).
    pub invites_enabled: bool,    /// SMTP provider URL (`smtp://` or `smtps://`). `None` (default) means
    /// outbound mail is explicitly disabled: confirmation/reset sends fail
    /// with `503 Email delivery is not configured`, never a silent success.
    pub smtp_url: Option<String>,
    /// Envelope sender for outbound mail. Required when `smtp_url` is set;
    /// setting it without `smtp_url` is a configuration error.
    pub mail_from: Option<String>,
    /// Whether `GET /api/v1/instance/peers` serves the known remote domains
    /// (upstream `Setting.peers_api_enabled`, default true). File-only, no
    /// CLI flag yet (same posture as `trends_enabled`).
    pub peers_api_enabled: bool,
    /// Whether `GET /api/v1/instance/activity` serves weekly local activity
    /// (upstream `Setting.activity_api_enabled`, default true). File-only.
    pub activity_api_enabled: bool,
    /// Whether `GET /api/v1/instance/domain_blocks` serves the user-facing
    /// domain-block list (upstream `Setting.show_domain_blocks`, default
    /// `disabled`). Unupstream granularity (`users`/`all` + rationale scope)
    /// is not modeled; `true` serves silence/suspend rows with public
    /// digests. File-only.
    pub domain_blocks_public: bool,
    /// Live-feed access gates (upstream `Setting.{local,remote}_live_feed_access`,
    /// default `public`). Each is `public`, `authenticated` or `disabled`
    /// (upstream `FEED_ACCESS_MODES`); anything but `public` requires login
    /// on `GET /api/v1/timelines/public`, and `disabled` additionally serves
    /// `[]` to non-staff. File-only.
    pub local_live_feed_access: String,
    pub remote_live_feed_access: String,
    /// Topic-feed access gates (upstream `Setting.{local,remote}_topic_feed_access`,
    /// default `public`). `local_*` is `public` or `authenticated` (upstream
    /// `ALTERNATE_FEED_ACCESS_MODES`); `remote_*` adds `disabled`. Gate
    /// `GET /api/v1/timelines/tag/*` and `/api/v1/timelines/link` the same
    /// way as the live gates. File-only.
    pub local_topic_feed_access: String,
    pub remote_topic_feed_access: String,
    /// Filesystem root for durable (local-upload) media bytes. `None`
    /// (default) means `<data_dir>/media` (the pre-overhaul path, kept as the
    /// durable root; remote rows with bytes there stay readable).
    pub durable_dir: Option<PathBuf>,
    /// Filesystem root for the remote-media cache. `None` (default) means
    /// `<data_dir>/media_cache` (new root; never holds durable bytes).
    pub cache_dir: Option<PathBuf>,
    /// Remote-media cache budgets + lifecycle (`docs/database.md` Step 0).
    pub remote_cache: RemoteCacheConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_url: "http://localhost:3000".to_string(),
            data_dir: PathBuf::from("./data"),
            listen: "0.0.0.0:3000".parse().unwrap(),
            trusted_proxies: Vec::new(),
            max_file_size: 16 * 1024 * 1024,
            max_body_size: 16 * 1024 * 1024,
            max_status_chars: 500,
            worker_poll_interval: Duration::from_millis(250),
            registration_open: false,
            registration_requires_approval: false,
            site_title: "Birchtree".to_string(),
            site_description: "A Mastodon-compatible server".to_string(),
            site_contact_email: "admin@localhost".to_string(),
            site_contact_username: None,
            streaming_api_base_url: None,
            federation_allow_http: false,
            federation_allowlist: Vec::new(),
            federation_extra_ca: None,
            federation_authorized_fetch: false,
            rate_limit_multiplier: 1,
            trends_enabled: true,
            search_mode: crate::search::SearchMode::Embedded,
            transcoding_enabled: true,
            ffmpeg_path: "ffmpeg".to_string(),
            webp_quality: 80,
            vp9_crf: 30,
            vp9_cpu_used: 2,
            opus_bitrate_kbps: 96,
            transcode_wall_time_secs: 120,
            transcode_max_output_bytes: 64 * 1024 * 1024,
            transcode_max_memory_bytes: 1024 * 1024 * 1024,
            vapid_public_key: String::new(),
            vapid_private_key: String::new(),
            web_client_dir: None,
            invites_enabled: true,
            smtp_url: None,
            mail_from: None,
            peers_api_enabled: true,
            activity_api_enabled: true,
            domain_blocks_public: false,
            local_live_feed_access: "public".to_string(),
            remote_live_feed_access: "public".to_string(),
            local_topic_feed_access: "public".to_string(),
            remote_topic_feed_access: "public".to_string(),
            durable_dir: None,
            cache_dir: None,
            remote_cache: RemoteCacheConfig::default(),
        }
    }
}

impl Config {
    /// Validate configuration. Returns a human-readable error string.
    pub fn validate(&self) -> Result<(), String> {
        if self.public_url.trim().is_empty() {
            return Err("--public-url must not be empty".to_string());
        }
        if !(self.public_url.starts_with("http://") || self.public_url.starts_with("https://")) {
            return Err("--public-url must start with http:// or https://".to_string());
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err("--data-dir must not be empty".to_string());
        }
        if self.max_file_size == 0 {
            return Err("max_file_size must be > 0".to_string());
        }
        if self.max_body_size == 0 {
            return Err("max_body_size must be > 0".to_string());
        }
        if self.max_file_size > self.max_body_size as u64 {
            return Err("max_file_size must not exceed max_body_size".to_string());
        }
        if self.max_status_chars == 0 {
            return Err("max_status_chars must be > 0".to_string());
        }
        if self.max_status_chars > 3000 {
            return Err("max_status_chars must not exceed 3000".to_string());
        }
        if self.worker_poll_interval.is_zero() {
            return Err("worker_poll_interval must be > 0".to_string());
        }
        if self.transcode_wall_time_secs == 0 {
            return Err("transcode_wall_time_secs must be > 0".to_string());
        }
        if self.transcode_max_output_bytes == 0 {
            return Err("transcode_max_output_bytes must be > 0".to_string());
        }
        if self.transcode_max_memory_bytes == 0 {
            return Err("transcode_max_memory_bytes must be > 0".to_string());
        }
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
        // Feed access gates mirror upstream's `FEED_ACCESS_MODES`
        // (`public`/`authenticated`/`disabled`) and
        // `ALTERNATE_FEED_ACCESS_MODES` (`public`/`authenticated`, no
        // `disabled` for the local topic feed).
        for (key, value) in [
            ("local_live_feed_access", &self.local_live_feed_access),
            ("remote_live_feed_access", &self.remote_live_feed_access),
            ("remote_topic_feed_access", &self.remote_topic_feed_access),
        ] {
            if !["public", "authenticated", "disabled"].contains(&value.as_str()) {
                return Err(format!(
                    "{key} must be one of public, authenticated, disabled"
                ));
            }
        }
        if !["public", "authenticated"].contains(&self.local_topic_feed_access.as_str()) {
            return Err(
                "local_topic_feed_access must be one of public, authenticated".to_string(),
            );
        }
        if let Some(base) = &self.streaming_api_base_url {
            let valid = ["ws://", "wss://", "http://", "https://"]
                .iter()
                .any(|scheme| base.starts_with(scheme) && base.len() > scheme.len());
            if !valid {
                return Err(
                    "--streaming-url must start with ws://, wss://, http:// or https://".to_string(),
                );
            }
        }
        // Mail: a half-configured provider is a hard startup error, never a
        // silent pass (round 3, area A). Unconfigured mail is a valid state —
        // sends then fail explicitly with 503 at the HTTP layer.
        match (&self.smtp_url, &self.mail_from) {
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
        // Remote-media cache budgets (`docs/database.md` Step 0): caps > 0,
        // distinct roots, global covers the reserved classes. The 40 MiB
        // per-file ceiling (`federation::media::MAX_REMOTE_MEDIA_BYTES`)
        // stays inside the class/global budgets (not a config key).
        if self.remote_cache.global_cap_bytes == 0 {
            return Err("remote_cache.global_cap_bytes must be > 0".to_string());
        }
        if self.remote_cache.avatar_emoji_cap_bytes == 0 {
            return Err("remote_cache.avatar_emoji_cap_bytes must be > 0".to_string());
        }
        if self.remote_cache.preview_cap_bytes == 0 {
            return Err("remote_cache.preview_cap_bytes must be > 0".to_string());
        }
        if self.remote_cache.full_cap_bytes == 0 {
            return Err("remote_cache.full_cap_bytes must be > 0".to_string());
        }
        if self.remote_cache.avatar_retention_days == 0 {
            return Err("remote_cache.avatar_retention_days must be > 0".to_string());
        }
        let reserved = self
            .remote_cache
            .avatar_emoji_cap_bytes
            .saturating_add(self.remote_cache.preview_cap_bytes)
            .saturating_add(self.remote_cache.full_cap_bytes);
        if self.remote_cache.global_cap_bytes < reserved {
            return Err("remote_cache.global_cap_bytes must cover the reserved class caps".to_string());
        }
        if self.cache_dir_resolved() == self.durable_dir_resolved() {
            return Err("cache_dir and durable_dir must be different directories".to_string());
        }
        Ok(())
    }

    /// Whether outbound mail has a configured provider.
    pub fn mail_enabled(&self) -> bool {
        self.smtp_url.is_some()
    }

    /// Load a JSON config file over the defaults. Unknown keys, unreadable
    /// files and malformed JSON are hard errors naming the path. Call
    /// [`Config::resolve_env_indirect`] afterwards (CLI flags first).
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|err| format!("--config {}: {err}", path.display()))?;
        serde_json::from_slice(&bytes)
            .map_err(|err| format!("--config {}: {err}", path.display()))
    }

    /// Resolve `env:VAR_NAME` indirections on the secret-carrying fields
    /// (`smtp_url`, `vapid_private_key`): the value is read from the
    /// environment so operators need not store secrets in the config file.
    /// Unset variables are a hard error naming the variable.
    pub fn resolve_env_indirect(&mut self) -> Result<(), String> {
        let smtp_indirect = self
            .smtp_url
            .as_deref()
            .and_then(|url| url.strip_prefix("env:"))
            .map(str::to_string);
        if let Some(name) = smtp_indirect {
            let value = std::env::var(&name)
                .map_err(|_| format!("config: env var {name} is not set (referenced by smtp_url)"))?;
            self.smtp_url = Some(value);
        }
        let vapid_indirect = self
            .vapid_private_key
            .strip_prefix("env:")
            .map(str::to_string);
        if let Some(name) = vapid_indirect {
            let value = std::env::var(&name)
                .map_err(|_| format!("config: env var {name} is not set (referenced by vapid_private_key)"))?;
            self.vapid_private_key = value;
        }
        Ok(())
    }

    /// Normalize the public URL (strip trailing slash) for actor/object URIs.
    pub fn normalized_public_url(&self) -> String {
        self.public_url.trim_end_matches('/').to_string()
    }

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
            config
                .validate()
                .map_err(|e| format!("transcoding: {e}"))?;
        }
        Ok(config)
    }

    /// Path of the `redb` database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("birchtree.redb")
    }

    /// Durable (local-upload) media root. Explicit `durable_dir` wins;
    /// otherwise `<data_dir>/media` (the pre-overhaul path).
    pub fn durable_dir_resolved(&self) -> PathBuf {
        self.durable_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("media"))
    }

    /// Remote-cache media root. Explicit `cache_dir` wins; otherwise
    /// `<data_dir>/media_cache`.
    pub fn cache_dir_resolved(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("media_cache"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        // Transcoding is on by default, which needs a working ffmpeg; the
        // hermetic default (unit tests, smoke scripts) disables it.
        let cfg = Config {
            transcoding_enabled: false,
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn transcoding_requires_a_working_ffmpeg() {
        // A missing binary is a startup error, never a silent passthrough.
        let cfg = Config {
            ffmpeg_path: "/nonexistent/birchtree-ffmpeg-xyz".into(),
            ..Config::default()
        };
        assert!(cfg.transcode_config().is_err());
        let off = Config {
            transcoding_enabled: false,
            ..Config::default()
        };
        assert!(off.transcode_config().is_ok());
    }

    #[test]
    fn rejects_non_http_public_url() {
        let cfg = Config {
            public_url: "ftp://example.social".into(),
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_zero_body_size() {
        let cfg = Config {
            max_body_size: 0,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_status_limit_is_mastodon_parity() {
        assert_eq!(Config::default().max_status_chars, 500);
    }

    #[test]
    fn rejects_zero_status_limit() {
        let cfg = Config {
            max_status_chars: 0,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_status_limit_above_misskey_parity() {
        let cfg = Config {
            max_status_chars: 3001,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
        let cfg = Config {
            max_status_chars: 3000,
            transcoding_enabled: false,
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn normalized_url_strips_trailing_slash() {
        let cfg = Config {
            public_url: "https://example.social/".into(),
            ..Config::default()
        };
        assert_eq!(cfg.normalized_public_url(), "https://example.social");
    }

    #[test]
    fn remote_cache_defaults_match_plan_budgets() {
        let cfg = Config::default();
        assert_eq!(cfg.remote_cache.mode, RemoteCacheMode::OnDemand);
        assert_eq!(cfg.remote_cache.global_cap_bytes, 512 * 1024 * 1024);
        assert_eq!(cfg.remote_cache.avatar_emoji_cap_bytes, 128 * 1024 * 1024);
        assert_eq!(cfg.remote_cache.preview_cap_bytes, 256 * 1024 * 1024);
        assert_eq!(cfg.remote_cache.full_cap_bytes, 128 * 1024 * 1024);
        assert_eq!(cfg.remote_cache.avatar_retention_days, 30);
        assert!(!cfg.remote_cache.warming);
        assert!(cfg.validate().is_ok());
        // Distinct default roots; durable keeps the pre-overhaul path.
        assert_ne!(cfg.cache_dir_resolved(), cfg.durable_dir_resolved());
        assert_eq!(cfg.durable_dir_resolved(), cfg.data_dir.join("media"));
        assert_eq!(cfg.cache_dir_resolved(), cfg.data_dir.join("media_cache"));
    }

    #[test]
    fn remote_cache_round_trips_through_json() {
        let cfg = Config {
            transcoding_enabled: false,
            ..Config::default()
        };
        let json = serde_json::to_value(&cfg).expect("serializes");
        let back: Config = serde_json::from_value(json).expect("round-trips");
        assert_eq!(
            back.remote_cache.global_cap_bytes,
            cfg.remote_cache.global_cap_bytes
        );
        assert!(back.validate().is_ok());
    }

    #[test]
    fn remote_cache_rejects_bad_budgets_and_shared_root() {
        let mut cfg = Config {
            transcoding_enabled: false,
            ..Config::default()
        };
        cfg.remote_cache.global_cap_bytes = 1;
        assert!(cfg.validate().is_err());
        let mut cfg = Config {
            transcoding_enabled: false,
            ..Config::default()
        };
        cfg.cache_dir = Some(cfg.durable_dir_resolved());
        assert!(cfg.validate().is_err());
    }
}
