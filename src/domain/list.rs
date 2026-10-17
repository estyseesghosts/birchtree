//! Lists and list membership.
//!
//! Mirrors Mastodon's `List` / `ListAccount`: a list belongs to one account,
//! membership requires an active follow (or pending request) with the list
//! owner, and unfollowing removes the member from the owner's lists
//! (`list_accounts.follow_id` is `ON DELETE CASCADE` upstream).

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// `List#replies_policy` (`enum :replies_policy, {list: 0, followed: 1,
/// none: 2}`): which replies may appear in the list timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepliesPolicy {
    /// Replies to list members.
    #[default]
    List,
    /// Replies to followed accounts.
    Followed,
    /// No replies beyond own/self replies.
    None,
}

impl RepliesPolicy {
    /// Wire form used by `REST::ListSerializer`.
    pub const fn as_str(self) -> &'static str {
        match self {
            RepliesPolicy::List => "list",
            RepliesPolicy::Followed => "followed",
            RepliesPolicy::None => "none",
        }
    }

    /// Parse the wire value; `None` for anything else (validation errors).
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "list" => Some(RepliesPolicy::List),
            "followed" => Some(RepliesPolicy::Followed),
            "none" => Some(RepliesPolicy::None),
            _ => None,
        }
    }
}

/// A list owned by one account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
    #[serde(default)]
    pub replies_policy: RepliesPolicy,
    #[serde(default)]
    pub exclusive: bool,
    #[serde(default)]
    pub created_at_ms: u64,
}

impl List {
    /// Mastodon `List::TITLE_LENGTH_LIMIT`.
    pub const TITLE_LENGTH_LIMIT: usize = 256;
    /// Mastodon `List::PER_ACCOUNT_LIMIT`.
    pub const PER_ACCOUNT_LIMIT: usize = 50;
}

/// One member row of a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMembership {
    pub list_id: Id,
    pub account_id: Id,
    /// True when the member was added under a pending follow request rather
    /// than an active follow.
    #[serde(default)]
    pub via_request: bool,
}
