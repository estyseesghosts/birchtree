//! Annual reports (Phase 7.3).
//!
//! Generation is a durable job ([`crate::domain::JobKind::GenerateAnnualReport`]);
//! the reads use `read|read:accounts` and generate/mark-read use
//! `write|write:accounts`. Report data is computed from real local records
//! (the author's statuses of that year); a year with no data reports honest
//! zeros rather than fabricated content.

use serde_json::Value;

use crate::domain::Id;

/// Schema version of [`AnnualReport::data`] as produced by this server.
///
/// This is birchtree's own shape (documented below), not Mastodon's
/// `GeneratedAnnualReport` schema: the wire serializer emits the same field
/// names (`year`, `data`, `schema_version`, `share_url`, `account_id`) with
/// `data` holding real local stats.
pub const ANNUAL_REPORT_SCHEMA_VERSION: u32 = 1;

/// A generated annual report for one account and year.
///
/// `data` holds:
/// - `statuses_count`: statuses authored that year;
/// - `favourites_count` / `reblogs_count`: sums of the received counters on
///   those statuses;
/// - `top_statuses`: up to five status ids (decimal strings), most
///   interacted-with first;
/// - `top_hashtags`: up to ten `{name, uses}` pairs from those statuses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnualReport {
    pub id: Id,
    pub account_id: Id,
    pub year: i32,
    pub data: Value,
    pub schema_version: u32,
    /// Sharing is out of profile (no public wrapstodon route), so this stays
    /// `None` and the serializer renders `share_url: null`.
    pub share_key: Option<String>,
    /// `None` = pending (shown in the index); `Some` = read (hidden).
    pub viewed_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// Input for inserting a generated report (idempotent on account + year).
#[derive(Debug, Clone)]
pub struct NewAnnualReport {
    pub account_id: Id,
    pub year: i32,
    pub data: Value,
    pub schema_version: u32,
}
