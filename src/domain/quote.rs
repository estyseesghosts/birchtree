//! Quotes: the normalized internal representation of a status quoting another.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Lifecycle state of a quote.
///
/// Mirrors Mastodon v4.7.3 `Quote.states`: `pending 0, accepted 1, rejected 2,
/// revoked 3, deleted 4`. Local quotes are accepted at creation when the
/// quoted status's policy allows; `rejected` is a transitional state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteState {
    Pending,
    Accepted,
    Rejected,
    Revoked,
    Deleted,
}

/// A quote linking a quoting status to the quoted status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRecord {
    pub id: Id,
    /// The status that quotes.
    pub quoting_status_id: Id,
    /// The status being quoted.
    pub quoted_status_id: Id,
    pub state: QuoteState,
    /// The quoting status's author.
    pub quoting_account_id: Id,
    /// The quoted status's author.
    pub quoted_account_id: Id,
    /// Quote activity URI (federation, Phase 6).
    pub quote_activity_uri: Option<String>,
    /// Approval URI (remote quotes, Phase 6).
    pub approval_uri: Option<String>,
    pub created_at_ms: u64,
}

impl QuoteRecord {
    /// True when the quote is accepted (counted and rendered).
    pub fn is_accepted(&self) -> bool {
        self.state == QuoteState::Accepted
    }
}
