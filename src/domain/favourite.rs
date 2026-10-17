//! Favourites (likes, unique per account/status pair).

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// A favourite (like).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favourite {
    /// Unique favourite id (used as the `favourited_by` pagination cursor).
    pub id: Id,
    pub account_id: Id,
    pub status_id: Id,
    pub created_at_ms: u64,
}
