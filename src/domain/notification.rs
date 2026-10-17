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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: Id,
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub read: bool,
    pub created_at_ms: u64,
}
