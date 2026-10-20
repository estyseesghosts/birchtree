//! Trend candidates and approvals (Phase 7.3).
//!
//! Public trend reads serve only admin-approved items (`allowed == Some(true)`);
//! unreviewed (`None`) and rejected (`Some(false)`) rows never appear there.
//! Candidate rows are materialized from real local usage (hashtag index,
//! recent public statuses, URLs found in status text) — never fabricated.

use crate::domain::Id;

/// Approval flag for a tag trend, keyed by normalized tag name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrendTagFlag {
    /// Numeric row id (the admin `:id`); minted when the row materializes.
    pub id: Id,
    /// Normalized tag name (lowercase, no `#` prefix).
    pub name: String,
    /// `None` = pending review; `Some(true)` = approved; `Some(false)` = rejected.
    pub allowed: Option<bool>,
    pub reviewed_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// Approval flag for a status trend, keyed by the status id itself.
///
/// Rows exist only after an admin review; absence of a row means pending.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrendStatusFlag {
    pub status_id: Id,
    /// `true` = approved (public when visible); `false` = rejected.
    pub allowed: bool,
    pub reviewed_at_ms: u64,
}

/// A link trend candidate (upstream `PreviewCard` + trend state).
///
/// `title`/`description`/provider fields stay `None` until the preview-card
/// crawler lands (planned gap, recorded in the endpoint manifest): an
/// approved link without fetched metadata serves `null` card fields rather
/// than fabricated content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrendLink {
    /// Numeric row id (the admin `:id`); minted when the row materializes.
    pub id: Id,
    /// The linked URL, exactly as found in status text.
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub provider_name: Option<String>,
    pub provider_url: Option<String>,
    pub image_url: Option<String>,
    /// `None` = pending review; `Some(true)` = approved; `Some(false)` = rejected.
    pub allowed: Option<bool>,
    pub reviewed_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// A link-publisher (preview-card provider) trend row, keyed by host.
///
/// Rows materialize alongside link rows: every link URL's host gets a
/// publisher row, so the admin publishers index lists every seen host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrendPublisher {
    /// Numeric row id (the admin `:id`); minted when the row materializes.
    pub id: Id,
    /// Lowercased host (no port) of the link URLs it provides.
    pub domain: String,
    /// `None` = pending review; `Some(true)` = approved; `Some(false)` = rejected.
    pub allowed: Option<bool>,
    pub reviewed_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// Distinct-tag usage derived from the hashtag index (candidate input, not a
/// stored row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagUsage {
    pub name: String,
    pub uses: u64,
}
