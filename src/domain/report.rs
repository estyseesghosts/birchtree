//! Reports (Flag): a report against accounts and statuses, local or received
//! from a remote server. The REST surface belongs to Phase 7.3; Phase 6 stores
//! reports from inbound `Flag` and sends outbound `Flag` for forwarded reports.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// One report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub id: Id,
    /// The account that filed the report (local, or remote for an inbound Flag).
    pub reporter_id: Id,
    /// The account the report is about.
    pub target_account_id: Id,
    /// Reported statuses of the target account.
    pub status_ids: Vec<Id>,
    /// Free text from the reporter (Mastodon caps it at 5000 characters).
    pub comment: String,
    /// Whether the report was forwarded to the target's server.
    pub forwarded: bool,
    /// The Flag activity's URI for a received report; `None` for a local report.
    pub activity_uri: Option<String>,
    pub created_at_ms: u64,
    // --- Phase 7.3 moderator queue (defaulted so pre-7.3 rows decode). ---
    /// Whether a moderator resolved the report (`action_taken`).
    #[serde(default)]
    pub action_taken: bool,
    /// When the report was resolved (`action_taken_at`).
    #[serde(default)]
    pub action_taken_at_ms: Option<u64>,
    /// The moderator who resolved it (`action_taken_by_account_id`).
    #[serde(default)]
    pub action_taken_by_account_id: Option<Id>,
    /// The moderator the report is assigned to (`assigned_account_id`).
    #[serde(default)]
    pub assigned_account_id: Option<Id>,
    /// Report category (`spam`, `legal`, `other`, ...; upstream default `other`).
    #[serde(default = "default_category")]
    pub category: String,
    /// Instance-rule ids attached at update time (validated against the
    /// rule store by the endpoint; upstream `Report#rules`).
    #[serde(default)]
    pub rule_ids: Vec<String>,
    /// Remote domains this report was forwarded to (upstream `forwarded` +
    /// `Report#forward_to_origin?` domain set).
    #[serde(default)]
    pub forward_to_domains: Vec<String>,
    /// Collection ids attached at creation (upstream `report_params`).
    #[serde(default)]
    pub collection_ids: Vec<u64>,
    /// Last moderator-queue write (`updated_at`).
    #[serde(default)]
    pub updated_at_ms: u64,
}

fn default_category() -> String {
    "other".to_string()
}
