use serde::{Deserialize, Serialize};

use crate::domain::{Id, Result};

/// Dismissed follow suggestions (fix round P1/S1; upstream
/// `follow_recommendation_mutes`). One row per (viewer, target) dismissal;
/// the ranking engine reads the set back into its exclusion list.
pub trait SuggestionStore: Send + Sync {
    /// Record a dismissal; returns `true` when the row is new.
    fn dismiss_suggestion(&self, viewer_id: Id, target_id: Id, now_ms: u64) -> Result<bool>;
    /// Dismissed target ids for `viewer_id`, bounded by `cap`.
    fn dismissed_suggestions(&self, viewer_id: Id, cap: usize) -> Result<Vec<Id>>;
}

/// A tag follow (`TagFollow`): unique per (account, normalized tag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagFollow {
    pub id: Id,
    pub account_id: Id,
    pub tag: String,
    pub created_at_ms: u64,
}

/// A featured tag on an account's profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturedTag {
    pub id: Id,
    pub account_id: Id,
    pub tag: String,
    pub created_at_ms: u64,
}

/// A session-bound first-party web push subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPushSubscription {
    pub id: Id,
    pub user_id: Id,
    pub endpoint: String,
    pub key_auth: String,
    pub key_p256dh: String,
    pub standard: bool,
    pub data: String,
    pub unsubscribe_token: String,
    pub created_at_ms: u64,
}
