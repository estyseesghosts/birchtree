//! Admin account moderation and moderator report queue use cases (Phase 7.3).
//!
//! Thin orchestration over [`AdminStore`]: the endpoints authenticate and
//! serialize; these functions enforce the local-account and category/type
//! rules and translate store results into domain errors. No HTTP or `redb`
//! types here.

use super::accounts::InviteStore;
use crate::application::ports::{
    AdminAccountFilter, AdminAccountPatch, AdminAccountStatus, AdminReportFilter, AdminReportPatch,
    AdminInfraStore, AdminStore, OAuthStore, Store, StreamingSink,
};
use crate::application::streaming::StreamEvent;
use crate::domain::{Account, Error, Id, LocalUser, Report, Result, Rule};

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
    text: Option<String>,
    now_ms: u64,
) -> Result<()> {
    store.admin_account_action(actor, account_id, action, report_id, text, now_ms)
}

/// D24: kill an account's sessions after its suspension or disablement
/// commits. Revokes every live token of the account in one transaction, then
/// — after commit — publishes one `TokenRevoked` per token so the hub closes
/// their open streams (upstream's `kill`: `Account::Suspensions#suspend!`
/// publishes `timeline:system:{id}` `{event: :kill}`, `User#disable!` and
/// `User#revoke_access!` do the same per token).
///
/// Revocation is permanent: `enable`/`unsuspend`/`approve` do NOT restore
/// tokens (upstream likewise never un-revokes). A killed token answers
/// `401 Invalid access token` on REST and streaming reuse.
///
/// Call this AFTER every commit that newly suspends or disables an account:
/// `DELETE`, `reject`, the `disable`/`suspend` action types, and patches that
/// set `suspended`/`disabled`. The D36 approval-queue and D38 deletion-worker
/// paths must call it too when they land — a suspend/disable without a kill
/// leaves the account's streams open (register D24).
pub fn kill_account_tokens(
    store: &dyn Store,
    sink: &dyn StreamingSink,
    account_id: Id,
) -> Result<usize> {
    let store: &dyn OAuthStore = store;
    let revoked = store.revoke_account_tokens(account_id)?;
    for token_id in &revoked {
        sink.publish(StreamEvent::TokenRevoked { token_id: *token_id });
    }
    Ok(revoked.len())
}

/// Validate a report category and rule-id combination.
///
/// A non-`violation` category with rule ids is rejected (upstream:
/// `rule_ids` must be absent). A `violation` category keeps the ids on the
/// row; their existence is validated separately by [`load_rules`] (upstream
/// `validate_rule_ids`).
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

/// Resolve report rule ids against the rule store, in order (D37).
///
/// Every id must parse and reference an existing rule, like upstream's
/// `validate_rule_ids` (`rules.size == rule_ids.size`); otherwise the
/// update is refused `422` (`reports.errors.invalid_rules`).
pub fn load_rules(store: &dyn AdminInfraStore, rule_ids: &[String]) -> Result<Vec<Rule>> {
    let mut ids = Vec::with_capacity(rule_ids.len());
    for raw in rule_ids {
        let id: Id = raw.parse().map_err(|_| {
            Error::InvalidInput("Validation failed: Rule ids does not reference valid rules".into())
        })?;
        ids.push(id);
    }
    let rules = store.admin_rules_by_ids(&ids)?;
    if rules.len() != ids.len() {
        return Err(Error::InvalidInput(
            "Validation failed: Rule ids does not reference valid rules".into(),
        ));
    }
    Ok(rules)
}

/// Create one server rule (operator-managed, like custom emoji).
pub fn create_rule(store: &dyn AdminInfraStore, text: &str, hint: &str, now_ms: u64) -> Result<Rule> {
    store.admin_create_rule(text, hint, now_ms)
}

/// Delete one server rule. Reports that referenced it keep the stale id;
/// serialization skips ids with no rule row.
pub fn delete_rule(store: &dyn AdminInfraStore, id: Id) -> Result<bool> {
    store.admin_delete_rule(id)
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

// ---------------------------------------------------------------------------
// Invited-by attribution (fix-4, P3: admin `invited_by` filter +
// `invited_by_account_id` serializer field).
// ---------------------------------------------------------------------------

/// Strictly parse an `invited_by` filter value to an inviter account id.
///
/// Upstream (`app/models/account_filter.rb:106-108` at `8ff11c2b`) scopes
/// with `Invite.where(user_id: value)`: a value naming no inviter matches
/// nothing. `None` therefore means "match nothing" (never "match
/// everything"); the endpoint layer maps it to an empty page.
///
/// Our invites record the issuing *account*
/// (`Invite.created_by_account_id`, `domain/user.rs`) where upstream's
/// `Invite` belongs to a *user* row; the account id is the faithful
/// equivalent here (one credential row per local account).
pub fn parse_invited_by(raw: &str) -> Option<Id> {
    raw.trim().parse::<Id>().ok()
}

/// Resolve an invite code to its issuing account.
///
/// This backs the `invited_by_account_id` serializer field: upstream renders
/// `user.invite.user.account_id`
/// (`app/serializers/rest/admin/account_serializer.rb:78-80` at `8ff11c2b`).
/// `None` for unknown codes (or store errors — attribution must never fail
/// the row; the field renders `null`).
pub fn inviter_account_id(invites: &dyn InviteStore, invite_code: &str) -> Option<Id> {
    match invites.invite_by_code(invite_code) {
        Ok(Some(invite)) => Some(invite.created_by_account_id),
        _ => None,
    }
}
