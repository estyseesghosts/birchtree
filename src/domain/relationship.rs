//! Relationship direction (row orientation for two-way lookups).

use crate::domain::Id;

/// Direction of a relationship row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// account A follows/blocks/mutes account B (A -> B)
    Outgoing,
    /// account B is followed/blocked/muted by account A (B <- A)
    Incoming,
}

/// A block row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub blocker_id: Id,
    pub blocked_id: Id,
    pub created_at_ms: u64,
}

/// A domain block row (viewer hides an entire domain).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DomainBlock {
    pub account_id: Id,
    pub domain: String,
    pub created_at_ms: u64,
}

/// A mute row (display suppression, not a privacy boundary).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Mute {
    pub muter_id: Id,
    pub muted_id: Id,
    /// Absolute expiry in ms; `None` = permanent.
    pub expire_at_ms: Option<u64>,
    /// Whether the muter also hides the muted account's notifications.
    pub notify: bool,
    pub created_at_ms: u64,
}

impl Mute {
    /// True when the mute suppresses notifications at `now_ms`.
    pub fn hides_notifications_at(&self, now_ms: u64) -> bool {
        self.notify && self.is_active_at(now_ms)
    }

    /// True while the mute has not expired at `now_ms`.
    pub fn is_active_at(&self, now_ms: u64) -> bool {
        match self.expire_at_ms {
            Some(expiry) => expiry > now_ms,
            None => true,
        }
    }
}

/// A private per-viewer note on an account (`RelationshipSerializer#note`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountNote {
    pub viewer_id: Id,
    pub target_id: Id,
    pub comment: String,
    pub updated_at_ms: u64,
}

/// An endorsement: an account "pins" another account to its profile.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Endorsement {
    pub account_id: Id,
    pub endorsed_id: Id,
    pub created_at_ms: u64,
}

/// Per-account counters, updated in the same transaction as the change that
/// moves them (follows, statuses, follow requests).
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct AccountStats {
    pub followers_count: u64,
    pub following_count: u64,
    pub statuses_count: u64,
    /// Date of the most recent non-deleted status (ms), for `last_status_at`.
    pub last_status_at_ms: Option<u64>,
    /// Pending inbound follow requests.
    pub follow_requests_count: u64,
}
