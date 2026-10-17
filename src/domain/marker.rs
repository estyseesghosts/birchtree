//! Per-user timeline read markers.
//!
//! Mirrors Mastodon's `Marker` (`timeline`, `last_read_id`, optimistic
//! `lock_version`): the notifications marker is what `unread_count` compares
//! against — Mastodon has no per-notification read flag.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// A per-account, per-timeline last-read marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    /// `home` or `notifications` (validated by the use case).
    pub timeline: String,
    pub last_read_id: Id,
    /// Optimistic-lock version; a stale update answers 409 like Mastodon.
    pub lock_version: u64,
    pub updated_at_ms: u64,
}

impl Marker {
    /// The timelines Mastodon accepts (`Marker::TIMELINES`).
    pub const TIMELINES: [&'static str; 2] = ["home", "notifications"];

    /// Validate a timeline name.
    pub fn timeline_valid(timeline: &str) -> bool {
        Self::TIMELINES.contains(&timeline)
    }
}
