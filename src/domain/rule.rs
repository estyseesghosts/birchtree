//! Server rules and account warnings (Phase 7.3, D37).
//!
//! Upstream (`app/models/rule.rb`, `app/models/account_warning.rb`):
//! a `Rule` carries `text` (max 300 chars) and a `hint`; reports attach
//! rule ids (`Report#rules`, `with_discarded`) and the admin report
//! serializer renders them. An `Admin::AccountAction` of any type records a
//! strike (`AccountWarning`: author, target, optional report, action, text,
//! status ids) and `none` resolves only its own report.
//!
//! Birchtree keeps the stored subset: rule `text` + `hint` (no translations,
//! no priority ordering, no soft-delete — a deleted rule id simply resolves
//! to nothing) and warnings as stored rows (no warning e-mail delivery, no
//! warning presets, no appeals).

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Maximum rule text length (upstream `Rule::TEXT_SIZE_LIMIT`).
pub const RULE_TEXT_LIMIT: usize = 300;

/// One server rule moderators can attach to reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: Id,
    pub text: String,
    #[serde(default)]
    pub hint: String,
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

/// One moderation warning (strike) against an account.
///
/// `action` is the account-action type that produced it (`none`, `disable`,
/// `sensitive`, `silence`, `suspend` — the same set as
/// `Admin::AccountAction::TYPES`, which is a subset of upstream's warning
/// action enum). `report_id` is always present here: a `none` action without
/// a report is refused `422` before a warning is built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountWarning {
    pub id: Id,
    /// The moderator account that issued the warning.
    pub account_id: Id,
    /// The account warned.
    pub target_account_id: Id,
    pub report_id: Option<Id>,
    pub action: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub status_ids: Vec<Id>,
    pub created_at_ms: u64,
}
