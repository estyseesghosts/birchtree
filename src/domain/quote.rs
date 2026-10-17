use crate::domain::Id;

/// Internal normalized quote representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuoteState {
    Pending,
    Accepted,
    Rejected,
    Revoked,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct QuoteRecord {
    pub id: Id,
    pub quoting_status_id: Id,
    pub quoted_status_id: Id,
    pub state: QuoteState,
    pub quote_activity_uri: Option<String>,
    pub approval_uri: Option<String>,
}
