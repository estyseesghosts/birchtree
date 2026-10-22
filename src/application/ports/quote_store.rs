use std::collections::HashMap;

use crate::domain::Id;
use crate::domain::QuoteRecord;
use crate::domain::Result;
use crate::domain::Status;

/// Quotes (local quote state machine; federated quote requests are Phase 6).
pub trait QuoteStore: Send + Sync {
    /// The quote authored by a quoting status, if any.
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>>;

    /// Quote records keyed by quoting status id in ONE transaction (page
    /// loader; statuses without a quote are omitted).
    fn quotes_by_quoting_ids(&self, ids: &[Id]) -> Result<HashMap<Id, QuoteRecord>>;

    /// Accepted quotes of `quoted_status_id` with their quoting statuses,
    /// newest first (cursor = quote id, upstream `paginate_by_max_id`).
    fn quotes_of_status(
        &self,
        quoted_status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(QuoteRecord, Status)>>;

    /// Revoke an accepted quote (quoted author only): state -> `revoked`,
    /// quoted counter decrement — ONE transaction. NotFound when no such
    /// quote; Forbidden handled by the use case before the call.
    fn revoke_quote(
        &self,
        quoted_status_id: Id,
        quoting_status_id: Id,
        now_ms: u64,
    ) -> Result<QuoteRecord>;
}
