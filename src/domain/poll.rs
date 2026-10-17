//! Polls, options and ballots.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// A poll attached to a status. Options and tallies are embedded so a read is
/// a single key lookup and a vote is a single-row atomic update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Poll {
    pub id: Id,
    pub status_id: Id,
    /// Author of the poll's status (cannot vote in own polls).
    pub account_id: Id,
    pub multiple: bool,
    /// Per-option `votes_count` is masked (serialized as `null`) until expiry.
    pub hide_totals: bool,
    /// Absolute expiry in ms; local polls always have one.
    pub expires_at_ms: Option<u64>,
    /// Sticky expiry flag (set by the `PollExpired` job; reads also treat
    /// `expires_at_ms <= now` as expired).
    pub expired: bool,
    pub options: Vec<PollOption>,
    /// Number of accounts that voted (ballots), regardless of choices.
    pub voters_count: u64,
    /// Total choices cast across all options.
    pub votes_count: u64,
}

impl Poll {
    /// True while the poll accepts votes at `now_ms`.
    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expired
            || match self.expires_at_ms {
                Some(expiry) => expiry <= now_ms,
                None => false,
            }
    }

    /// Whether per-option totals are visible at `now_ms`.
    pub fn show_totals_at(&self, now_ms: u64) -> bool {
        !self.hide_totals || self.is_expired_at(now_ms)
    }
}

/// One poll option with its tally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollOption {
    pub title: String,
    pub votes_count: u64,
}

/// One account's ballot on a poll (unique per poll/account).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollVote {
    pub poll_id: Id,
    pub account_id: Id,
    /// Chosen option indices (into the poll's `options` vector).
    pub choices: Vec<u64>,
    pub created_at_ms: u64,
}
