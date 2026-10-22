use crate::domain::AnnualReport;
use crate::domain::Id;
use crate::domain::NewAnnualReport;
use crate::domain::Result;
use crate::domain::Status;

/// Annual-report storage (generation runs as a durable job).
pub trait AnnualReportStore: Send + Sync {
    /// This account's report for `year`, if generated.
    fn annual_report(&self, account_id: Id, year: i32) -> Result<Option<AnnualReport>>;
    /// This account's unread reports, newest year first (the index).
    fn pending_annual_reports(&self, account_id: Id) -> Result<Vec<AnnualReport>>;
    /// Insert a generated report; idempotent on (account, year) — returns the
    /// existing row when one is already stored, so a retried generation job
    /// never duplicates it.
    fn insert_annual_report(&self, input: NewAnnualReport, now_ms: u64) -> Result<AnnualReport>;
    /// Mark the account's report for `year` read; `None` when missing.
    fn mark_annual_report_viewed(
        &self,
        account_id: Id,
        year: i32,
        now_ms: u64,
    ) -> Result<Option<AnnualReport>>;
    /// True when a `GenerateAnnualReport` job for (account, year) is still
    /// pending or leased (the `generating` state).
    fn has_pending_generate_report(&self, account_id: Id, year: i32) -> Result<bool>;
    /// The account's non-deleted statuses created in `year`, oldest first,
    /// bounded by `cap` (generation input; ids are time-ordered so the scan
    /// stops at the year boundary).
    fn account_statuses_in_year(
        &self,
        account_id: Id,
        year: i32,
        cap: usize,
    ) -> Result<Vec<Status>>;
}
