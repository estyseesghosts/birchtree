//! Admin account moderation and moderator report queue use cases (Phase 7.3).
//!
//! Thin orchestration over [`AdminStore`]: the endpoints authenticate and
//! serialize; these functions enforce the local-account and category/type
//! rules and translate store results into domain errors. No HTTP or `redb`
//! types here.

use crate::application::ports::{
    AdminAccountFilter, AdminAccountPatch, AdminAccountStatus, AdminReportFilter, AdminReportPatch,
    AdminStore,
};
use crate::domain::{Account, Error, Id, LocalUser, Report, Result};

/// Report categories upstream accepts (`app/models/report.rb`).
const REPORT_CATEGORIES: &[&str] = &["other", "spam", "legal", "violation"];

/// Account-action types upstream accepts (`Admin::AccountAction::TYPES`).
const ACTION_TYPES: &[&str] = &["none", "disable", "sensitive", "silence", "suspend"];

/// Actions that only make sense for local accounts (upstream
/// `require_local_account!` and `types_for_account`).
const LOCAL_ONLY_ACTIONS: &[&str] = &["enable", "approve", "reject", "disable", "none"];

/// The account must be local *and* have a credential row, mirroring
/// upstream's `require_local_account!` (`local? && user.present?`).
/// The caller loads both through `AccountStore`; this only checks.
pub fn require_local_target(account: &Account, user: Option<&LocalUser>) -> Result<()> {
    if !is_local_with_user(account, user) {
        return Err(Error::Forbidden("Forbidden".into()));
    }
    Ok(())
}

/// List accounts through the store (no additional policy: the gate ran).
pub fn list_accounts(
    store: &dyn AdminStore,
    filter: AdminAccountFilter,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    ascending: bool,
    limit: usize,
) -> Result<Vec<(Account, Option<LocalUser>)>> {
    store.admin_accounts_page(&filter, max_id, since_id, min_id, ascending, limit)
}

/// Status filter keyword from the v1 (`active`/`pending`/... present-flags)
/// and v2 (`status`) spellings. Unknown values are ignored (upstream's
/// `AccountFilter` case/when has no else branch).
pub fn parse_status(raw: Option<&str>) -> Option<AdminAccountStatus> {
    match raw {
        None => None,
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "active" => Some(AdminAccountStatus::Active),
            "pending" => Some(AdminAccountStatus::Pending),
            "disabled" => Some(AdminAccountStatus::Disabled),
            "sensitized" => Some(AdminAccountStatus::Sensitized),
            "silenced" => Some(AdminAccountStatus::Silenced),
            "suspended" => Some(AdminAccountStatus::Suspended),
            _ => None,
        },
    }
}

/// Whether an account row counts as local with a credential.
pub fn is_local_with_user(account: &Account, user: Option<&LocalUser>) -> bool {
    account.local && user.is_some()
}

/// Reject non-local targets for the local-only actions with upstream's
/// `require_local_account!` response (`403 {"error":"Forbidden"}`).
pub fn require_local_action_target(
    account: &Account,
    user: Option<&LocalUser>,
    _action: &str,
) -> Result<()> {
    require_local_target(account, user)
}

/// True when the action name is local-only.
pub fn is_local_only_action(action: &str) -> bool {
    LOCAL_ONLY_ACTIONS.contains(&action)
}

/// Validate an account-action type and its local-only restriction.
pub fn check_action_type(action: &str, local: bool) -> Result<()> {
    if !ACTION_TYPES.contains(&action) {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Type {action} is not included in the list"
        )));
    }
    if (action == "disable" || action == "none") && !local {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Type {action} is not included in the list"
        )));
    }
    Ok(())
}

/// Apply an account moderation patch with its audit action.
pub fn moderate_account(
    store: &dyn AdminStore,
    actor: Id,
    account_id: Id,
    patch: AdminAccountPatch,
    now_ms: u64,
) -> Result<(Account, Option<LocalUser>)> {
    store.admin_update_account(actor, account_id, patch, now_ms)
}

/// Run one account action (flags + implied report resolutions + audit rows in
/// one transaction).
pub fn account_action(
    store: &dyn AdminStore,
    actor: Id,
    account_id: Id,
    action: &str,
    report_id: Option<Id>,
    now_ms: u64,
) -> Result<()> {
    store.admin_account_action(actor, account_id, action, report_id, now_ms)
}

/// Validate a report category and rule-id combination.
///
/// Rules have no model yet: a non-`violation` category with rule ids is
/// rejected (upstream: `rule_ids` must be absent); a `violation` category
/// keeps the ids on the row but renders `rules: []` (documented gap).
pub fn check_report_patch(category: Option<&str>, rule_ids: Option<&[String]>) -> Result<()> {
    if let Some(category) = category {
        if !REPORT_CATEGORIES.contains(&category) {
            return Err(Error::InvalidInput(format!(
                "Validation failed: Category {category} is not included in the list"
            )));
        }
        if category != "violation" && rule_ids.is_some_and(|ids| !ids.is_empty()) {
            return Err(Error::InvalidInput(
                "Validation failed: Rule ids must be blank".into(),
            ));
        }
    } else if rule_ids.is_some_and(|ids| !ids.is_empty()) {
        return Err(Error::InvalidInput(
            "Validation failed: Rule ids must be blank".into(),
        ));
    }
    Ok(())
}

/// Update one queued report with its audit action.
pub fn update_report(
    store: &dyn AdminStore,
    actor: Id,
    id: Id,
    patch: AdminReportPatch,
    now_ms: u64,
) -> Result<Option<Report>> {
    store.admin_update_report(actor, id, patch, now_ms)
}

/// List queued reports through the store.
pub fn list_reports(
    store: &dyn AdminStore,
    filter: AdminReportFilter,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    ascending: bool,
    limit: usize,
) -> Result<Vec<Report>> {
    store.admin_reports_page(&filter, max_id, since_id, min_id, ascending, limit)
}
