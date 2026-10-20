//! Admin infrastructure records (Phase 7.3, subagent C).
//!
//! Server-level moderation rows managed through `/api/v1/admin/*`: domain
//! allows and blocks, email-domain blocks, IP blocks, canonical email blocks,
//! admin tag flags, measures/dimensions/retention inputs, and the audit trail.
//!
//! Enforcement notes (planned gaps, see `docs/endpoint-manifest.md`):
//! - Domain blocks are enforced at federation ingress/egress (Phase 6).
//! - Email-domain and canonical-email blocks need email confirmation
//!   (Phase 2 gap) before they can gate registration.
//! - IP blocks are enforced in the request path through the trusted-proxy
//!   client IP (`admin_infra` endpoint module provides the guard).

use crate::domain::Id;

/// A domain allow row (limited-federation mode: only listed domains federate).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminDomainAllow {
    pub id: Id,
    pub domain: String,
    pub created_at_ms: u64,
}

/// Severity of a server-level domain block (upstream `DomainBlock` enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminDomainSeverity {
    Silence,
    Suspend,
    Noop,
}

impl AdminDomainSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Suspend => "suspend",
            Self::Noop => "noop",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "silence" => Some(Self::Silence),
            "suspend" => Some(Self::Suspend),
            "noop" => Some(Self::Noop),
            _ => None,
        }
    }
}

/// A server-level domain block row (upstream `DomainBlock`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminDomainBlock {
    pub id: Id,
    pub domain: String,
    pub severity: AdminDomainSeverity,
    pub reject_media: bool,
    pub reject_reports: bool,
    pub private_comment: Option<String>,
    pub public_comment: Option<String>,
    pub obfuscate: bool,
    pub created_at_ms: u64,
}

/// An email-domain block row (upstream `EmailDomainBlock`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminEmailDomainBlock {
    pub id: Id,
    pub domain: String,
    pub allow_with_approval: bool,
    pub created_at_ms: u64,
}

/// Severity of an IP block (upstream `IpBlock` enum, integer-backed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdminIpSeverity {
    SignUpRequiresApproval,
    SignUpBlock,
    NoAccess,
}

impl AdminIpSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignUpRequiresApproval => "sign_up_requires_approval",
            Self::SignUpBlock => "sign_up_block",
            Self::NoAccess => "no_access",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "sign_up_requires_approval" => Some(Self::SignUpRequiresApproval),
            "sign_up_block" => Some(Self::SignUpBlock),
            "no_access" => Some(Self::NoAccess),
            _ => None,
        }
    }
}

/// An IP block row (upstream `IpBlock`; `ip` is CIDR text, e.g. `1.2.3.0/24`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminIpBlock {
    pub id: Id,
    pub ip: String,
    pub severity: AdminIpSeverity,
    pub comment: String,
    /// Absolute expiry in ms; `None` = permanent.
    pub expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// A canonical-email block row (upstream `CanonicalEmailBlock`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminCanonicalEmailBlock {
    pub id: Id,
    /// Hex-encoded SHA-256 of the canonicalized email.
    pub canonical_email_hash: String,
    pub created_at_ms: u64,
}

/// Admin-managed tag flags (upstream `REST::Admin::TagSerializer`).
///
/// Rows are materialized from real hashtag usage (see the application module):
/// the name is authoritative from the hashtag index; the flags are operator
/// state with Mastodon-compatible `None` (unset) semantics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminTag {
    pub id: Id,
    pub name: String,
    pub display_name: String,
    pub trendable: Option<bool>,
    pub usable: Option<bool>,
    pub listable: Option<bool>,
    pub reviewed_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

/// One audit-trail record per admin write (proposed default: on).
///
/// Minimal shape; subagent B may extend it for accounts/reports writes — the
/// table is shared, the `target_type` distinguishes rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdminAuditRecord {
    pub id: Id,
    pub actor_account_id: Id,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub created_at_ms: u64,
}

/// One account row projected for instance-scoped metrics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdminAccountOverview {
    pub id: Id,
    pub created_at_ms: u64,
    pub domain: String,
    pub local: bool,
    /// Bare username (D39 `instance_accounts` dimension keys by username,
    /// like upstream).
    pub username: String,
}

/// One report row projected for instance-scoped metrics.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct AdminReportTarget {
    pub created_at_ms: u64,
    pub target_account_id: Id,
}

/// One media row projected for instance-scoped metrics.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct AdminMediaOwner {
    pub created_at_ms: u64,
    pub file_size: u64,
    pub owner_id: Id,
}

/// One follow row projected for instance-scoped metrics
/// (`instance_follows` counts follows by *target* domain,
/// `instance_followers` by *follower* domain — upstream
/// `InstanceFollowsMeasure` / `InstanceFollowersMeasure`).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct FollowMetricRow {
    pub follower_id: Id,
    pub target_id: Id,
    pub created_at_ms: u64,
}
