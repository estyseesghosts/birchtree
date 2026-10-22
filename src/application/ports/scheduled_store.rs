use crate::domain::{Id, Result, ScheduledStatus};

/// A scheduled status to create in ONE transaction (record + indexes + the
/// `PublishScheduledStatus` job).
#[derive(Debug, Clone)]
pub struct NewScheduledStatus {
    pub account_id: Id,
    pub scheduled_at_ms: u64,
    pub media_ids: Vec<Id>,
    pub params: serde_json::Value,
    /// Public URL base, carried into the job payload so the publish handler
    /// can replay the params through the normal creation use case.
    pub base_url: String,
}

pub trait ScheduledStore: Send + Sync {
    fn create_scheduled(&self, input: NewScheduledStatus, now_ms: u64) -> Result<ScheduledStatus>;
    fn scheduled_by_id(&self, id: Id) -> Result<Option<ScheduledStatus>>;
    /// The account's scheduled statuses, id-descending with cursors.
    fn scheduled_by_account(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<ScheduledStatus>>;
    /// Reschedule (only `scheduled_at` is editable); `None` when
    /// missing/foreign.
    fn reschedule(
        &self,
        account_id: Id,
        id: Id,
        scheduled_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ScheduledStatus>>;
    /// Delete a scheduled status (publish job payload becomes a no-op).
    fn delete_scheduled(&self, account_id: Id, id: Id) -> Result<bool>;
    /// Rows due for publication at `now_ms` (bounded).
    fn due_scheduled(&self, now_ms: u64, cap: usize) -> Result<Vec<ScheduledStatus>>;
    /// `(total, today)` counts against Mastodon's 300/25 limits.
    fn scheduled_counts(
        &self,
        account_id: Id,
        day_start_ms: u64,
        day_end_ms: u64,
    ) -> Result<(usize, usize)>;
}
