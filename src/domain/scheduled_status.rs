//! Scheduled statuses.
//!
//! Mirrors Mastodon's `ScheduledStatus`: a row of *replayable* params plus
//! attached media ids. Publishing destroys the row first (media detach via
//! `dependent: :nullify`) and then replays the params through the normal
//! status-creation use case, so a scheduled post goes through identical
//! validation, counters, notifications and jobs as an immediate one.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// A status scheduled for later publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledStatus {
    pub id: Id,
    pub account_id: Id,
    /// Absolute publication time (ms).
    pub scheduled_at_ms: u64,
    /// Media attached to the scheduled status, in order.
    #[serde(default)]
    pub media_ids: Vec<Id>,
    /// Replayable creation params (text, visibility, spoiler, language,
    /// sensitive, poll spec, reply/quote targets, quote policy).
    pub params: serde_json::Value,
    #[serde(default)]
    pub created_at_ms: u64,
}

impl ScheduledStatus {
    /// Mastodon `ScheduledStatus::TOTAL_LIMIT`.
    pub const TOTAL_LIMIT: usize = 300;
    /// Mastodon `ScheduledStatus::DAILY_LIMIT`.
    pub const DAILY_LIMIT: usize = 25;
    /// Mastodon `ScheduledStatus::MINIMUM_OFFSET` (5 minutes).
    pub const MINIMUM_OFFSET_MS: u64 = 5 * 60 * 1_000;
}
