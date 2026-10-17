//! Follow relationships and follow requests.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Lifecycle state of a follow row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowState {
    /// An accepted follow.
    Active,
    /// A pending follow request against a locked (or otherwise gated) account.
    Pending,
}

/// A follow or follow-request row (unique per follower/followed pair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Follow {
    pub follower_id: Id,
    pub followed_id: Id,
    /// Whether the follower sees the target's reblogs.
    pub show_reblogs: bool,
    /// Whether the follower wants follow notifications.
    pub notify: bool,
    pub state: FollowState,
    pub created_at_ms: u64,
    /// Remote follow activity URI (Phase 6); `None` for local follows.
    pub uri: Option<String>,
}
