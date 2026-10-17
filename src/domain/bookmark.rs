//! Bookmarks (private saves, unique per account/status pair).

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// A bookmark (private save).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    /// Unique bookmark id (pagination cursor for the bookmarks list).
    pub id: Id,
    pub account_id: Id,
    pub status_id: Id,
    pub created_at_ms: u64,
}
