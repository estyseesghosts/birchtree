use crate::domain::{Account, Id, Result, Status};

/// Bounded local search scans backing `GET /api/v2/search`.
pub trait SearchStore: Send + Sync {
    /// Accounts whose lowercase handle or display name contains `needle`
    /// (bounded).
    fn search_accounts(&self, needle: &str, cap: usize) -> Result<Vec<Account>>;

    /// Distinct hashtags whose normalized name starts with `prefix`
    /// (bounded; derived from the hashtag index).
    fn search_tags(&self, prefix: &str, cap: usize) -> Result<Vec<String>>;

    /// Recent statuses (optionally one author's), id-descending with
    /// cursors, capped (bounded recent scan for text search).
    fn recent_statuses(
        &self,
        account_id: Option<Id>,
        max_id: Option<Id>,
        min_id: Option<Id>,
        cap: usize,
    ) -> Result<Vec<Status>>;
}
