//! Durable notification records.
//!
//! Records are created by the `FanOutNotification` job *after* the interaction
//! commits, through a dedup index so a replayed job never duplicates a
//! notification. The read surface (index, grouping, markers) is Phase 4.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Notification kinds exposed by the API (`type` is the snake_case wire form).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    Mention,
    Follow,
    Reblog,
    Favourite,
    PollVote,
    FollowRequest,
    Quote,
}

impl NotificationType {
    /// Stable wire/log form (`REST::NotificationSerializer#type`).
    pub const fn as_str(self) -> &'static str {
        match self {
            NotificationType::Mention => "mention",
            NotificationType::Follow => "follow",
            NotificationType::Reblog => "reblog",
            NotificationType::Favourite => "favourite",
            NotificationType::PollVote => "poll",
            NotificationType::FollowRequest => "follow_request",
            NotificationType::Quote => "quote",
        }
    }

    /// Ordinal used in the dedup key (stable per variant order).
    pub const fn ord(self) -> u64 {
        match self {
            NotificationType::Mention => 1,
            NotificationType::Follow => 2,
            NotificationType::Reblog => 3,
            NotificationType::Favourite => 4,
            NotificationType::PollVote => 5,
            NotificationType::FollowRequest => 6,
            NotificationType::Quote => 7,
        }
    }
}

/// A notification row.
///
/// `filtered` mirrors upstream's `notifications.filtered`: a filtered row is
/// hidden from the index/`unread_count` and reachable only through its
/// notification request until the recipient accepts (unfilter) or dismisses
/// (delete) it. Rows written before the request pipeline read as `false`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: Id,
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub read: bool,
    #[serde(default)]
    pub filtered: bool,
    pub created_at_ms: u64,
}

/// One arm of a notification policy (`NotificationPolicy.for_*` upstream,
/// `accept`/`filter`/`drop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Accept,
    Filter,
    Drop,
}

impl PolicyAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            PolicyAction::Accept => "accept",
            PolicyAction::Filter => "filter",
            PolicyAction::Drop => "drop",
        }
    }

    pub fn parse(raw: &str) -> Option<PolicyAction> {
        match raw {
            "accept" => Some(PolicyAction::Accept),
            "filter" => Some(PolicyAction::Filter),
            "drop" => Some(PolicyAction::Drop),
            _ => None,
        }
    }
}

/// A recipient's notification policy (upstream `NotificationPolicy`,
/// `notification_policies` table, one row per account).
///
/// Defaults match upstream's column defaults: private mentions and limited
/// accounts filter, everything else accepts. `Default` is the same as
/// "no stored row" (`find_or_initialize_by`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPolicy {
    pub account_id: Id,
    pub for_not_following: PolicyAction,
    pub for_not_followers: PolicyAction,
    pub for_new_accounts: PolicyAction,
    pub for_private_mentions: PolicyAction,
    pub for_limited_accounts: PolicyAction,
    pub for_bots: PolicyAction,
}

impl NotificationPolicy {
    pub fn new(account_id: Id) -> Self {
        Self {
            account_id,
            for_not_following: PolicyAction::Accept,
            for_not_followers: PolicyAction::Accept,
            for_new_accounts: PolicyAction::Accept,
            for_private_mentions: PolicyAction::Filter,
            for_limited_accounts: PolicyAction::Filter,
            for_bots: PolicyAction::Accept,
        }
    }

    /// V1 compat (`REST::V1::NotificationPolicySerializer`): the
    /// `filter_*` booleans are "not accept".
    pub fn filter_not_following(&self) -> bool {
        self.for_not_following != PolicyAction::Accept
    }

    pub fn filter_not_followers(&self) -> bool {
        self.for_not_followers != PolicyAction::Accept
    }

    pub fn filter_new_accounts(&self) -> bool {
        self.for_new_accounts != PolicyAction::Accept
    }

    pub fn filter_private_mentions(&self) -> bool {
        self.for_private_mentions != PolicyAction::Accept
    }

    pub fn filter_bots(&self) -> bool {
        self.for_bots != PolicyAction::Accept
    }
}

/// A notification request: filtered notifications from one actor, pending the
/// recipient's accept (unfilter + `notifications_merged`) or dismiss
/// (upstream `NotificationRequest`; `last_status` is the newest filtered
/// mention/quote status, `notifications_count` the filtered-row count).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRequest {
    pub id: Id,
    pub account_id: Id,
    pub from_account_id: Id,
    pub last_status_id: Option<Id>,
    pub notifications_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
