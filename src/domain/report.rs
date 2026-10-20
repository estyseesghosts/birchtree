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
}
