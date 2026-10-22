//! Remote-media cache configuration (S9 section of `crate::config`).
//!
//! Verbatim move of `RemoteCacheMode` (`config.rs:21-39`) and
//! `RemoteCacheConfig` (`config.rs:95-128`). The parent cutover deletes the
//! originals and re-exports these. No behavior change.

/// Remote-media cache backing mode (`docs/remote-media-cache-plan.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCacheMode {
    /// No persistence: startup refuses this mode until a non-persisting
    /// streaming/proxy delivery path exists (adversarial-2 F1). The ledger
    /// refuses every reservation and commit loudly while disabled.
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
