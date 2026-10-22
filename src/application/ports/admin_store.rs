use crate::domain::{Account, AdminAuditRecord, Id, LocalUser, Report, Result};

// ---------------------------------------------------------------------------
// Phase 7.3 ports: admin accounts moderation and the moderator report queue.
//
// Same invariants as the other ports: one `redb` transaction per method;
// mutating methods commit the record change, its index/counter updates and
// the audit-trail row atomically.
// ---------------------------------------------------------------------------

/// Moderation status filter for the admin account index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAccountStatus {
    /// No moderation flag and not disabled.
    Active,
    /// Awaiting approval: local accounts whose credential row has
    /// `approved == false` (upstream `User.pending`). Remote accounts, which
    /// have no credential row, never match.
    Pending,
    Disabled,
    Sensitized,
    Silenced,
    Suspended,
}

/// Filters for the admin account index. The v1 and v2 endpoints map their
/// (different) upstream parameter names onto this one struct.
#[derive(Debug, Clone, Default)]
pub struct AdminAccountFilter {
    /// `Some(true)` = local only, `Some(false)` = remote only, `None` = either.
    pub local_only: Option<bool>,
    /// Substring match on the account domain (`by_domain`).
    pub by_domain: Option<String>,
    /// Moderation status (`active`/`pending`/`disabled`/`sensitized`/
    /// `silenced`/`suspended`, v2 `status`).
    pub status: Option<AdminAccountStatus>,
    /// Substring match on the username (`username`).
    pub username_sub: Option<String>,
    /// Substring match on the display name (`display_name`).
    pub display_name_sub: Option<String>,
    /// Substring match on the credential email (`email`; local accounts only).
    pub email_sub: Option<String>,
    /// Substring match on a login IP (`ip`). Matches when any recorded login
    /// IP contains the substring (fix round P1/P3; IPs recorded since schema
    /// v15, older rows have none).
    pub ip_sub: Option<String>,
    /// Only staff accounts (`staff`, v2 `permissions=staff`).
    pub staff_only: bool,
    /// Only accounts created with an invite issued by this account (v2
    /// `invited_by`; upstream `AccountFilter#invited_by`). `None` = no
    /// constraint. A garbage `invited_by` value parses to `None` at the
    /// endpoint and matches nothing (see `index_v2`).
    pub invited_by: Option<Id>,
    /// v2 `role_ids`: matched against the role name (`admin`/`moderator`).
    /// Birchtree has no numeric role ids (documented gap).
    pub role_names: Vec<String>,
}

/// Field-level admin account patch. `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct AdminAccountPatch {
    pub suspended: Option<bool>,
    pub silenced: Option<bool>,
    pub sensitized: Option<bool>,
    pub disabled: Option<bool>,
    /// Approval state (`approve` sets `Some(true)`; upstream `User#approve!`
    /// also enables the account, which the endpoint expresses as
    /// `disabled: Some(false)` alongside).
    pub approved: Option<bool>,
    /// Audit action name recorded with the write (`enable`, `suspend`, ...).
    pub audit_action: String,
}

/// Filters for the moderator report queue.
#[derive(Debug, Clone, Default)]
pub struct AdminReportFilter {
    /// `Some(true)` = resolved only, `Some(false)` = unresolved only.
    pub resolved: Option<bool>,
    /// Only reports filed by this account (`account_id`).
    pub reporter_id: Option<Id>,
    /// Only reports about this account (`target_account_id`).
    pub target_account_id: Option<Id>,
}

/// Field-level moderator report patch. `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct AdminReportPatch {
    /// New category (`spam`, `legal`, `other`, ...).
    pub category: Option<String>,
    /// Attached rule ids. A non-empty list is validated against the rule
    /// store (unknown ids are refused `422`, like upstream
    /// `validate_rule_ids`); empty clears nothing (stored as given when
    /// empty).
    pub rule_ids: Option<Vec<String>>,
    /// `Some(None)` unassigns; `None` leaves the assignee unchanged.
    pub assigned_account_id: Option<Option<Id>>,
    /// Resolve (`Some(true)`) or reopen (`Some(false)`).
    pub action_taken: Option<bool>,
    /// The moderator resolving; set when resolving.
    pub action_taken_by_account_id: Option<Id>,
    /// Audit action name recorded with the write (`update`, `resolve`, ...).
    pub audit_action: String,
}

/// Admin account moderation and the moderator report queue.
pub trait AdminStore: Send + Sync {
    /// Admin account index, id-descending with exclusive cursors, bounded:
    /// at most `limit` rows after scanning at most `limit * 20` (capped at
    /// 5000) index entries. Each row carries its credential (`None` for
    /// remote accounts, which have no local user).
    fn admin_accounts_page(
        &self,
        filter: &AdminAccountFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<(Account, Option<LocalUser>)>>;

    /// Apply an account moderation patch in ONE transaction (account row,
    /// credential row when `disabled` changes, and the audit row). `NotFound`
    /// when the account is missing.
    fn admin_update_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        patch: AdminAccountPatch,
        now_ms: u64,
    ) -> Result<(Account, Option<LocalUser>)>;

    /// Suspend and disable an account in ONE transaction (the audit row
    /// records `audit_action`: `destroy` for DELETE, `reject` for reject),
    /// and enqueue the async deletion worker's first batch plus the
    /// federated `Delete` of the actor in the same transaction (D38).
    /// Returns `false` when the account is missing. The account's statuses
    /// are removed by the bounded, restartable batch chain, not here; the
    /// account row itself is retained (suspended) so its URI resolves.
    fn admin_delete_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        audit_action: &str,
        now_ms: u64,
    ) -> Result<bool>;

    /// One account-action write in ONE transaction: the moderation flags, the
    /// warning row it records (every action type records a strike, like
    /// upstream `process_strike!`; `text` is the moderator's warning text),
    /// the report resolutions it implies, and one audit row for the action
    /// plus one per resolved report. `NotFound` when the account (or the
    /// named report) is missing.
    fn admin_account_action(
        &self,
        actor_account_id: Id,
        account_id: Id,
        action: &str,
        report_id: Option<Id>,
        text: Option<String>,
        now_ms: u64,
    ) -> Result<()>;

    /// Moderator report queue, id-descending with exclusive cursors, bounded
    /// like [`AdminStore::admin_accounts_page`].
    fn admin_reports_page(
        &self,
        filter: &AdminReportFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<Report>>;

    /// Apply a moderator report patch in ONE transaction (report row + audit
    /// row). `Ok(None)` when the report is missing.
    fn admin_update_report(
        &self,
        actor_account_id: Id,
        id: Id,
        patch: AdminReportPatch,
        now_ms: u64,
    ) -> Result<Option<Report>>;

    /// Audit records, newest first, bounded by `cap` (test and operator reads).
    fn admin_audit_entries(&self, cap: usize) -> Result<Vec<AdminAuditRecord>>;

    /// Warning rows (strikes) against `target_account_id`, newest first
    /// (test and operator reads; upstream serves warnings through the
    /// dashboard only, with no admin API read).
    fn admin_warnings_for_account(
        &self,
        target_account_id: Id,
    ) -> Result<Vec<crate::domain::AccountWarning>>;
}
