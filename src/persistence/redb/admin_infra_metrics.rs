//! Bounded admin metric scans (S6 slice of `admin_infra`).
//!
//! Read-only full-table scans, each bounded by `METRIC_SCAN_CAP` index
//! entries (shared with the other `admin_infra_*` slices via
//! [`super::admin_scan_ctx`]) so a dashboard query can never scan without
//! bound. One `redb` read transaction per method.

use redb::ReadableTable;

use crate::application::ports::StatusMetricRow;
use crate::domain::{
    Account, AdminAccountOverview, AdminMediaOwner, AdminReportTarget, Follow, FollowMetricRow, Id,
    MediaAttachment, Report, Result, SignInEvent, Status,
};

use super::admin_scan_ctx::METRIC_SCAN_CAP;
use super::schema::{ACCOUNTS, FOLLOWS, MEDIA, REPORTS, SIGN_INS, STATUSES};
use super::{RedbStore, decode};

// -- Bounded metric scans --

pub(super) fn admin_account_created_times(store: &RedbStore, cap: usize) -> Result<Vec<u64>> {
    let read = store.begin_read()?;
    let accounts = read.open_table(ACCOUNTS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in accounts.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let account: Account = decode(value.value())?;
        out.push(account.created_at_ms);
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    out.sort_unstable();
    Ok(out)
}

pub(super) fn admin_status_metrics(store: &RedbStore, cap: usize) -> Result<Vec<StatusMetricRow>> {
    let read = store.begin_read()?;
    let statuses = read.open_table(STATUSES)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in statuses.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let status: Status = decode(value.value())?;
        if status.deleted_at_ms.is_some() {
            continue;
        }
        out.push(StatusMetricRow {
            account_id: status.account_id,
            created_at_ms: status.created_at_ms,
            language: status.language.clone(),
            application_name: status.application.as_ref().map(|app| app.name.clone()),
            is_reblog: status.is_reblog(),
        });
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    out.sort_by_key(|row| row.created_at_ms);
    Ok(out)
}

pub(super) fn admin_report_created_times(store: &RedbStore, cap: usize) -> Result<Vec<u64>> {
    let read = store.begin_read()?;
    let reports = read.open_table(REPORTS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in reports.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let report: Report = decode(value.value())?;
        out.push(report.created_at_ms);
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    out.sort_unstable();
    Ok(out)
}

pub(super) fn admin_report_resolved_times(store: &RedbStore, cap: usize) -> Result<Vec<u64>> {
    let read = store.begin_read()?;
    let reports = read.open_table(REPORTS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in reports.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let report: Report = decode(value.value())?;
        if let Some(at) = report.action_taken_at_ms {
            out.push(at);
        }
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    out.sort_unstable();
    Ok(out)
}

pub(super) fn admin_media_metrics(store: &RedbStore, cap: usize) -> Result<Vec<(u64, u64)>> {
    let read = store.begin_read()?;
    let media = read.open_table(MEDIA)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in media.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let attachment: MediaAttachment = decode(value.value())?;
        out.push((attachment.created_at_ms, attachment.file_size));
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    out.sort_by_key(|&(created, _)| created);
    Ok(out)
}

pub(super) fn admin_remote_domains(store: &RedbStore, cap: usize) -> Result<Vec<(String, u64)>> {
    let read = store.begin_read()?;
    let accounts = read.open_table(ACCOUNTS)?;
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut scanned = 0usize;
    for entry in accounts.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let account: Account = decode(value.value())?;
        if !account.local {
            *counts.entry(account.domain.clone()).or_insert(0) += 1;
        }
    }
    store.note_scanned(scanned);
    let mut out: Vec<(String, u64)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(cap.max(1));
    Ok(out)
}

pub(super) fn admin_local_domain(_store: &RedbStore) -> String {
    // The store does not carry `ServerInfo`; the endpoint supplies the
    // domain for instance-scoped metric params. The trait method exists so
    // callers behind `&dyn Store` share one spelling.
    String::new()
}

/// Round 3 (D39 `space_usage`): the live `redb` file size, measured from
/// the filesystem â€” never estimated. `None` when the file cannot be
/// statted (the caller omits the entry). postgres/redis sizes are never
/// reported: this server has neither.
pub(super) fn admin_db_size_bytes(store: &RedbStore) -> Result<Option<u64>> {
    match std::fs::metadata(store.path()) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(_) => Ok(None),
    }
}

pub(super) fn admin_accounts_overview(
    store: &RedbStore,
    cap: usize,
) -> Result<Vec<AdminAccountOverview>> {
    let read = store.begin_read()?;
    let accounts = read.open_table(ACCOUNTS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in accounts.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let account: Account = decode(value.value())?;
        out.push(AdminAccountOverview {
            id: account.id,
            created_at_ms: account.created_at_ms,
            domain: account.domain,
            local: account.local,
            username: account.username,
        });
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_report_targets(
    store: &RedbStore,
    cap: usize,
) -> Result<Vec<AdminReportTarget>> {
    let read = store.begin_read()?;
    let reports = read.open_table(REPORTS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in reports.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let report: Report = decode(value.value())?;
        out.push(AdminReportTarget {
            created_at_ms: report.created_at_ms,
            target_account_id: report.target_account_id,
        });
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_media_owners(store: &RedbStore, cap: usize) -> Result<Vec<AdminMediaOwner>> {
    let read = store.begin_read()?;
    let media = read.open_table(MEDIA)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in media.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let attachment: MediaAttachment = decode(value.value())?;
        out.push(AdminMediaOwner {
            created_at_ms: attachment.created_at_ms,
            file_size: attachment.file_size,
            owner_id: attachment.account_id,
        });
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_follow_metrics(store: &RedbStore, cap: usize) -> Result<Vec<FollowMetricRow>> {
    let read = store.begin_read()?;
    let follows = read.open_table(FOLLOWS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in follows.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let follow: Follow = decode(value.value())?;
        out.push(FollowMetricRow {
            follower_id: follow.follower_id,
            target_id: follow.followed_id,
            created_at_ms: follow.created_at_ms,
        });
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_sign_ins(store: &RedbStore, cap: usize) -> Result<Vec<(Id, u64)>> {
    let read = store.begin_read()?;
    let table = read.open_table(SIGN_INS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in table.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > METRIC_SCAN_CAP {
            break;
        }
        let event: SignInEvent = decode(value.value())?;
        out.push((event.account_id, event.created_at_ms));
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}
