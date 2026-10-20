//! Annual reports (Phase 7.3).
//!
//! Generation is a durable [`crate::domain::JobKind::GenerateAnnualReport`]
//! job; the handler computes real stats from the author's statuses of that
//! year and inserts the row idempotently. A year with no data reports honest
//! zeros rather than fabricated content.

use std::collections::HashMap;

use crate::application::ports::Store;
use crate::domain::{AnnualReport, Id, JobKind, NewAnnualReport, Result, Status, ANNUAL_REPORT_SCHEMA_VERSION};

/// Generation states served by `GET /api/v1/annual_reports/:id/state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportState {
    Available,
    Generating,
    Eligible,
    Ineligible,
}

impl ReportState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportState::Available => "available",
            ReportState::Generating => "generating",
            ReportState::Eligible => "eligible",
            ReportState::Ineligible => "ineligible",
        }
    }
}

/// Outcome of `POST /api/v1/annual_reports/:id/generate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateOutcome {
    /// A report already exists (upstream answers `200 {}` here).
    Exists,
    /// A generation job is already running (`202`, like upstream's
    /// async-refresh path).
    Running,
    /// A generation job was enqueued (`202`).
    Enqueued,
}

/// The account's pending (unread) reports, newest year first.
pub fn index_reports(store: &dyn Store, viewer: Id) -> Result<Vec<AnnualReport>> {
    store.pending_annual_reports(viewer)
}

/// One report by year (`:id` is the year upstream); `NotFound` when missing.
pub fn show_report(store: &dyn Store, viewer: Id, year: i32) -> Result<AnnualReport> {
    store
        .annual_report(viewer, year)?
        .ok_or_else(|| crate::domain::Error::NotFound(format!("annual report {year}")))
}

/// Generation state for one year.
pub fn report_state(store: &dyn Store, viewer: Id, year: i32) -> Result<ReportState> {
    if store.annual_report(viewer, year)?.is_some() {
        return Ok(ReportState::Available);
    }
    if store.has_pending_generate_report(viewer, year)? {
        return Ok(ReportState::Generating);
    }
    if store
        .account_statuses_in_year(viewer, year, 1)?
        .is_empty()
    {
        return Ok(ReportState::Ineligible);
    }
    Ok(ReportState::Eligible)
}

/// Request generation of one year's report: existing rows and running jobs
/// are left alone; otherwise a durable job is enqueued (same-transaction
/// commit via the job queue, retried by the worker until it succeeds).
pub fn request_generate(store: &dyn Store, viewer: Id, year: i32, now_ms: u64) -> Result<GenerateOutcome> {
    if store.annual_report(viewer, year)?.is_some() {
        return Ok(GenerateOutcome::Exists);
    }
    if store.has_pending_generate_report(viewer, year)? {
        return Ok(GenerateOutcome::Running);
    }
    store.enqueue(
        JobKind::GenerateAnnualReport,
        serde_json::json!({
            "account_id": viewer.value(),
            "year": year,
        }),
        now_ms,
    )?;
    Ok(GenerateOutcome::Enqueued)
}

/// Mark the account's report for `year` read; `None` when missing (404).
pub fn mark_viewed(
    store: &dyn Store,
    viewer: Id,
    year: i32,
    now_ms: u64,
) -> Result<Option<AnnualReport>> {
    store.mark_annual_report_viewed(viewer, year, now_ms)
}

/// Statuses referenced by a report's `top_statuses`, in rank order (missing
/// or deleted rows are skipped, never fabricated).
pub fn report_statuses(store: &dyn Store, report: &AnnualReport) -> Result<Vec<Status>> {
    let mut out = Vec::new();
    if let Some(ids) = report.data.get("top_statuses").and_then(|v| v.as_array()) {
        for raw in ids {
            let Some(text) = raw.as_str() else {
                continue;
            };
            let Ok(id) = text.parse::<Id>() else {
                continue;
            };
            if let Some(status) = store.status_by_id(id)?
                && !status.is_deleted()
            {
                out.push(status);
            }
        }
    }
    Ok(out)
}

/// Compute a year's report data from real local records and store it
/// idempotently (the generation-job body; a retry after commit is a no-op).
pub fn run_generation_job(
    store: &dyn Store,
    account_id: Id,
    year: i32,
    now_ms: u64,
) -> Result<AnnualReport> {
    if let Some(existing) = store.annual_report(account_id, year)? {
        return Ok(existing);
    }
    let statuses = store.account_statuses_in_year(account_id, year, 20_000)?;
    store.insert_annual_report(
        NewAnnualReport {
            account_id,
            year,
            data: stats_data(&statuses),
            schema_version: ANNUAL_REPORT_SCHEMA_VERSION,
        },
        now_ms,
    )
}

/// Aggregate real per-year stats from the author's statuses.
fn stats_data(statuses: &[Status]) -> serde_json::Value {
    let mut favourites: u64 = 0;
    let mut reblogs: u64 = 0;
    let mut hashtags: HashMap<String, u64> = HashMap::new();
    for status in statuses {
        favourites += status.favourites_count;
        reblogs += status.reblogs_count;
        for tag in &status.tags {
            *hashtags.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<&Status> = statuses.iter().collect();
    ranked.sort_by(|a, b| {
        (b.favourites_count + b.reblogs_count)
            .cmp(&(a.favourites_count + a.reblogs_count))
            .then(b.id.cmp(&a.id))
    });
    let top_statuses: Vec<String> = ranked
        .into_iter()
        .take(5)
        .map(|status| status.id.to_string())
        .collect();
    let mut top_hashtags: Vec<(&String, &u64)> = hashtags.iter().collect();
    top_hashtags.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top_hashtags: Vec<serde_json::Value> = top_hashtags
        .into_iter()
        .take(10)
        .map(|(name, uses)| serde_json::json!({ "name": name, "uses": uses }))
        .collect();
    serde_json::json!({
        "statuses_count": statuses.len(),
        "favourites_count": favourites,
        "reblogs_count": reblogs,
        "top_statuses": top_statuses,
        "top_hashtags": top_hashtags,
    })
}
