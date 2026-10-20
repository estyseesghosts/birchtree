//! Admin account moderation and the moderator report queue (Phase 7.3).
//!
//! One `redb` transaction per method: the record mutation, the credential or
//! report row it touches, and the audit-trail row commit together
//! (`birchtree.md` §5). Reads are bounded scans (at most `limit * 20`,
//! capped at 5000, index entries) so an admin list can never scan without
//! bound.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{
    AdminAccountFilter, AdminAccountPatch, AdminAccountStatus, AdminReportFilter, AdminReportPatch,
    AdminStore,
};
use crate::domain::{
    Account, AdminAuditRecord, Error, Id, LocalUser, Report, Result, UserRole,
};

use super::schema::{ACCOUNTS, ADMIN_AUDIT, LOCAL_USERS, REPORTS};
use super::{decode, encode, RedbStore, StoreResult};

/// Maximum index entries walked to fill one admin page.
const SCAN_CAP: usize = 5000;

fn scan_budget(limit: usize) -> usize {
    (limit.saturating_mul(20)).clamp(limit, SCAN_CAP)
}

fn read_account_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Account>> {
    let table = write.open_table(ACCOUNTS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn read_user_txn(write: &WriteTransaction, account_id: Id) -> StoreResult<Option<LocalUser>> {
    let table = write.open_table(LOCAL_USERS)?;
    match table.get(account_id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn read_report_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Report>> {
    let table = write.open_table(REPORTS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn write_account_txn(write: &WriteTransaction, account: &Account) -> StoreResult<()> {
    let mut table = write.open_table(ACCOUNTS)?;
    table.insert(account.id.value(), encode(account)?.as_slice())?;
    Ok(())
}

fn write_user_txn(write: &WriteTransaction, user: &LocalUser) -> StoreResult<()> {
    let mut table = write.open_table(LOCAL_USERS)?;
    table.insert(user.account_id.value(), encode(user)?.as_slice())?;
    Ok(())
}

fn write_report_txn(write: &WriteTransaction, report: &Report) -> StoreResult<()> {
    let mut table = write.open_table(REPORTS)?;
    table.insert(report.id.value(), encode(report)?.as_slice())?;
    Ok(())
}

fn write_audit_txn(
    write: &WriteTransaction,
    audit_id: Id,
    actor_account_id: Id,
    action: &str,
    target_type: &str,
    target_id: &str,
    now_ms: u64,
) -> StoreResult<()> {
    let record = AdminAuditRecord {
        id: audit_id,
        actor_account_id,
        action: action.to_string(),
        target_type: target_type.to_string(),
        target_id: target_id.to_string(),
        created_at_ms: now_ms,
    };
    let mut table = write.open_table(ADMIN_AUDIT)?;
    table.insert(audit_id.value(), encode(&record)?.as_slice())?;
    Ok(())
}

fn role_matches(user: Option<&LocalUser>, filter: &AdminAccountFilter) -> bool {
    if filter.staff_only
        && user
            .map(|u| u.role == UserRole::None)
            .unwrap_or(true)
    {
        return false;
    }
    if !filter.role_names.is_empty() {
        let role = user.map(|u| u.role).unwrap_or(UserRole::None);
        let wanted = |name: &str| match name.to_ascii_lowercase().as_str() {
            "admin" => role == UserRole::Admin,
            "moderator" => role == UserRole::Moderator,
            _ => false,
        };
        if !filter.role_names.iter().any(|n| wanted(n)) {
            return false;
        }
    }
    true
}

fn status_matches(account: &Account, user: Option<&LocalUser>, status: AdminAccountStatus) -> bool {
    let disabled = user.map(|u| u.disabled).unwrap_or(false);
    match status {
        // No pending-approval state exists (Phase 2 gap), so nothing is pending.
        AdminAccountStatus::Pending => false,
        AdminAccountStatus::Active => {
            !account.suspended && !account.silenced && !account.sensitized && !disabled
        }
        AdminAccountStatus::Disabled => disabled,
        AdminAccountStatus::Sensitized => account.sensitized,
        AdminAccountStatus::Silenced => account.silenced,
        AdminAccountStatus::Suspended => account.suspended,
    }
}

fn account_matches(account: &Account, user: Option<&LocalUser>, filter: &AdminAccountFilter) -> bool {
    if let Some(local) = filter.local_only {
        if account.local != local {
            return false;
        }
    }
    if let Some(domain) = filter.by_domain.as_deref() {
        if account.domain.to_ascii_lowercase() != domain.to_ascii_lowercase() {
            return false;
        }
    }
    if let Some(status) = filter.status {
        if !status_matches(account, user, status) {
            return false;
        }
    }
    if let Some(needle) = filter.username_sub.as_deref() {
        if !account.username.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
            return false;
        }
    }
    if let Some(needle) = filter.display_name_sub.as_deref() {
        if !account.display_name.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
            return false;
        }
    }
    if let Some(needle) = filter.email_sub.as_deref() {
        let email = user.and_then(|u| u.email.as_deref()).unwrap_or("");
        if !email.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
            return false;
        }
    }
    // No login IPs are stored, so a non-empty `ip` filter matches nothing.
    if filter.ip_sub.as_deref().is_some_and(|s| !s.is_empty()) {
        return false;
    }
    if !role_matches(user, filter) {
        return false;
    }
    true
}

fn report_matches(report: &Report, filter: &AdminReportFilter) -> bool {
    // Upstream's resolved scope is `action_taken_at IS NOT NULL`.
    if let Some(resolved) = filter.resolved {
        if report.action_taken_at_ms.is_some() != resolved {
            return false;
        }
    }
    if let Some(reporter) = filter.reporter_id {
        if report.reporter_id != reporter {
            return false;
        }
    }
    if let Some(target) = filter.target_account_id {
        if report.target_account_id != target {
            return false;
        }
    }
    true
}

fn resolve_report(report: &mut Report, actor: Id, now_ms: u64) {
    report.action_taken = true;
    report.action_taken_at_ms = Some(now_ms);
    report.action_taken_by_account_id = Some(actor);
    report.updated_at_ms = now_ms;
}

impl AdminStore for RedbStore {
    fn admin_accounts_page(
        &self,
        filter: &AdminAccountFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<(Account, Option<LocalUser>)>> {
        let budget = scan_budget(limit.max(1));
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        let users = read.open_table(LOCAL_USERS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;

        if ascending {
            let start = min_id.map(|id| id.value() + 1).unwrap_or(0);
            for entry in accounts.range(start..)? {
                let (_, value) = entry?;
                if scanned >= budget {
                    break;
                }
                scanned += 1;
                let account: Account = decode(value.value())?;
                if let Some(max) = max_id {
                    if account.id >= max {
                        continue;
                    }
                }
                if let Some(since) = since_id {
                    if account.id <= since {
                        continue;
                    }
                }
                let user = match users.get(account.id.value())? {
                    Some(guard) => Some(decode::<LocalUser>(guard.value())?),
                    None => None,
                };
                if account_matches(&account, user.as_ref(), filter) {
                    out.push((account, user));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        } else {
            for entry in accounts.range(0u64..)?.rev() {
                let (key, value) = entry?;
                if scanned >= budget {
                    break;
                }
                scanned += 1;
                let id = Id::new(key.value());
                if let Some(max) = max_id {
                    if id >= max {
                        continue;
                    }
                }
                if let Some(since) = since_id {
                    if id <= since {
                        continue;
                    }
                }
                if let Some(min) = min_id {
                    if id <= min {
                        break;
                    }
                }
                let account: Account = decode(value.value())?;
                let user = match users.get(account.id.value())? {
                    Some(guard) => Some(decode::<LocalUser>(guard.value())?),
                    None => None,
                };
                if account_matches(&account, user.as_ref(), filter) {
                    out.push((account, user));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    fn admin_update_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        patch: AdminAccountPatch,
        now_ms: u64,
    ) -> Result<(Account, Option<LocalUser>)> {
        let write = self.begin_write()?;
        let mut account = read_account_txn(&write, account_id)?
            .ok_or_else(|| Error::NotFound(format!("account {account_id}")))?;
        if let Some(v) = patch.suspended {
            account.suspended = v;
        }
        if let Some(v) = patch.silenced {
            account.silenced = v;
        }
        if let Some(v) = patch.sensitized {
            account.sensitized = v;
        }
        let mut user = read_user_txn(&write, account_id)?;
        if let Some(disabled) = patch.disabled {
            let Some(record) = user.as_mut() else {
                return Err(Error::InvalidInput(
                    "Only local accounts can be enabled or disabled".into(),
                ));
            };
            record.disabled = disabled;
        }
        write_account_txn(&write, &account)?;
        if let Some(record) = &user {
            write_user_txn(&write, record)?;
        }
        write_audit_txn(
            &write,
            self.next_id(),
            actor_account_id,
            &patch.audit_action,
            "account",
            &account_id.to_string(),
            now_ms,
        )?;
        write.commit()?;
        Ok((account, user))
    }

    fn admin_delete_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        audit_action: &str,
        now_ms: u64,
    ) -> Result<bool> {
        let write = self.begin_write()?;
        let Some(mut account) = read_account_txn(&write, account_id)? else {
            return Ok(false);
        };
        // Immediate suspend + disable (no async deletion worker yet; the
        // account's statuses are left in place — a planned gap).
        account.suspended = true;
        write_account_txn(&write, &account)?;
        if let Some(mut user) = read_user_txn(&write, account_id)? {
            user.disabled = true;
            write_user_txn(&write, &user)?;
        }
        write_audit_txn(
            &write,
            self.next_id(),
            actor_account_id,
            audit_action,
            "account",
            &account_id.to_string(),
            now_ms,
        )?;
        write.commit()?;
        Ok(true)
    }

    fn admin_account_action(
        &self,
        actor_account_id: Id,
        account_id: Id,
        action: &str,
        report_id: Option<Id>,
        now_ms: u64,
    ) -> Result<()> {
        let write = self.begin_write()?;
        let mut account = read_account_txn(&write, account_id)?
            .ok_or_else(|| Error::NotFound(format!("account {account_id}")))?;
        let user = read_user_txn(&write, account_id)?;
        let local = account.local && user.is_some();

        match action {
            "none" => {
                // Warn-only actions have no warning/notification model yet.
                let Some(target) = report_id else {
                    return Err(Error::InvalidInput(
                        "Validation failed: Account warnings are not supported".into(),
                    ));
                };
                let mut report = read_report_txn(&write, target)?
                    .ok_or_else(|| Error::NotFound(format!("report {target}")))?;
                resolve_report(&mut report, actor_account_id, now_ms);
                write_report_txn(&write, &report)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "resolve",
                    "report",
                    &target.to_string(),
                    now_ms,
                )?;
            }
            "disable" => {
                if !local {
                    return Err(Error::InvalidInput(
                        "Validation failed: Only local accounts can be disabled".into(),
                    ));
                }
                let mut user = user.ok_or_else(|| Error::NotFound(format!("account {account_id}")))?;
                user.disabled = true;
                write_user_txn(&write, &user)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "disable",
                    "user",
                    &account_id.to_string(),
                    now_ms,
                )?;
            }
            "sensitive" => {
                account.sensitized = true;
                write_account_txn(&write, &account)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "sensitive",
                    "account",
                    &account_id.to_string(),
                    now_ms,
                )?;
            }
            "silence" => {
                account.silenced = true;
                write_account_txn(&write, &account)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "silence",
                    "account",
                    &account_id.to_string(),
                    now_ms,
                )?;
            }
            "suspend" => {
                account.suspended = true;
                write_account_txn(&write, &account)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "suspend",
                    "account",
                    &account_id.to_string(),
                    now_ms,
                )?;
            }
            other => {
                return Err(Error::InvalidInput(format!(
                    "Validation failed: Type {other} is not included in the list"
                )));
            }
        }

        // Non-`none` actions resolve every unresolved report about the target
        // (upstream `process_reports!`).
        if action != "none" {
            let table = write.open_table(REPORTS)?;
            let mut pending: Vec<Report> = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                let report: Report = decode(value.value())?;
                if report.target_account_id == account_id && report.action_taken_at_ms.is_none() {
                    pending.push(report);
                }
            }
            drop(table);
            for mut report in pending {
                let id = report.id;
                resolve_report(&mut report, actor_account_id, now_ms);
                write_report_txn(&write, &report)?;
                write_audit_txn(
                    &write,
                    self.next_id(),
                    actor_account_id,
                    "resolve",
                    "report",
                    &id.to_string(),
                    now_ms,
                )?;
            }
        }

        write.commit()?;
        Ok(())
    }

    fn admin_reports_page(
        &self,
        filter: &AdminReportFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<Report>> {
        let budget = scan_budget(limit.max(1));
        let read = self.begin_read()?;
        let table = read.open_table(REPORTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;

        if ascending {
            let start = min_id.map(|id| id.value() + 1).unwrap_or(0);
            for entry in table.range(start..)? {
                let (_, value) = entry?;
                if scanned >= budget {
                    break;
                }
                scanned += 1;
                let report: Report = decode(value.value())?;
                if let Some(max) = max_id {
                    if report.id >= max {
                        continue;
                    }
                }
                if let Some(since) = since_id {
                    if report.id <= since {
                        continue;
                    }
                }
                if report_matches(&report, filter) {
                    out.push(report);
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        } else {
            for entry in table.range(0u64..)?.rev() {
                let (key, value) = entry?;
                if scanned >= budget {
                    break;
                }
                scanned += 1;
                let id = Id::new(key.value());
                if let Some(max) = max_id {
                    if id >= max {
                        continue;
                    }
                }
                if let Some(since) = since_id {
                    if id <= since {
                        continue;
                    }
                }
                if let Some(min) = min_id {
                    if id <= min {
                        break;
                    }
                }
                let report: Report = decode(value.value())?;
                if report_matches(&report, filter) {
                    out.push(report);
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    fn admin_update_report(
        &self,
        actor_account_id: Id,
        id: Id,
        patch: AdminReportPatch,
        now_ms: u64,
    ) -> Result<Option<Report>> {
        let write = self.begin_write()?;
        let Some(mut report) = read_report_txn(&write, id)? else {
            return Ok(None);
        };
        if let Some(category) = patch.category {
            report.category = category;
        }
        if let Some(rule_ids) = patch.rule_ids {
            report.rule_ids = rule_ids;
        }
        if let Some(assigned) = patch.assigned_account_id {
            report.assigned_account_id = assigned;
        }
        match patch.action_taken {
            Some(true) => {
                report.action_taken = true;
                report.action_taken_at_ms = Some(now_ms);
                report.action_taken_by_account_id = patch.action_taken_by_account_id;
            }
            Some(false) => {
                report.action_taken = false;
                report.action_taken_at_ms = None;
                report.action_taken_by_account_id = None;
            }
            None => {}
        }
        report.updated_at_ms = now_ms;
        write_report_txn(&write, &report)?;
        write_audit_txn(
            &write,
            self.next_id(),
            actor_account_id,
            &patch.audit_action,
            "report",
            &id.to_string(),
            now_ms,
        )?;
        write.commit()?;
        Ok(Some(report))
    }

    fn admin_audit_entries(&self, cap: usize) -> Result<Vec<AdminAuditRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(ADMIN_AUDIT)?;
        let mut out = Vec::new();
        for entry in table.range(0u64..=u64::MAX)?.rev() {
            if out.len() >= cap {
                break;
            }
            let (_, value) = entry?;
            out.push(decode(value.value())?);
        }
        Ok(out)
    }
}
