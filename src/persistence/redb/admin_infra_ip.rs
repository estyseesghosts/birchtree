//! Admin IP blocks (S6 slice of `admin_infra`).
//!
//! Global ordered table keyed by `desc_id(row_id)` (forward scan is newest
//! first) plus the per-IP unique index. One `redb` transaction per method.

use redb::ReadableTable;

use crate::application::admin_infra as use_cases;
use crate::application::ports::AdminIpBlockPatch;
use crate::domain::{AdminIpBlock, AdminIpSeverity, Error, Id, Result};

use super::admin_scan_ctx::{PAGE_SCAN_CAP, get_by_desc, page_desc};
use super::schema::{self, ADMIN_IP_BLOCK_BY_IP, ADMIN_IP_BLOCKS};
use super::{RedbStore, decode, encode};

// -- IP blocks --

pub(super) fn admin_create_ip_block(
    store: &RedbStore,
    ip: &str,
    severity: AdminIpSeverity,
    comment: &str,
    expires_at_ms: Option<u64>,
    now_ms: u64,
) -> Result<AdminIpBlock> {
    let cidr = use_cases::parse_cidr(ip)?;
    let text = use_cases::cidr_text(cidr);
    let write = store.begin_write()?;
    let duplicate: bool = {
        let by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
        by_ip.get(text.as_str())?.is_some()
    };
    if duplicate {
        return Err(Error::InvalidInput(
            "Validation failed: Ip has already been taken".into(),
        ));
    }
    let row = AdminIpBlock {
        id: store.next_id(),
        ip: text.clone(),
        severity,
        comment: comment.to_string(),
        expires_at_ms,
        created_at_ms: now_ms,
    };
    {
        let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
        by_ip.insert(text.as_str(), row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_ip_block_by_id(store: &RedbStore, id: Id) -> Result<Option<AdminIpBlock>> {
    get_by_desc(store, ADMIN_IP_BLOCKS, id)
}

pub(super) fn admin_ip_blocks_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminIpBlock>> {
    page_desc(store, ADMIN_IP_BLOCKS, max_id, since_id, min_id, limit)
}

pub(super) fn admin_ip_blocks_all(store: &RedbStore, cap: usize) -> Result<Vec<AdminIpBlock>> {
    let read = store.begin_read()?;
    let rows = read.open_table(ADMIN_IP_BLOCKS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in rows.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > PAGE_SCAN_CAP {
            break;
        }
        out.push(decode::<AdminIpBlock>(value.value())?);
        if out.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_update_ip_block(
    store: &RedbStore,
    id: Id,
    patch: AdminIpBlockPatch,
    _now_ms: u64,
) -> Result<Option<AdminIpBlock>> {
    let existing: Option<AdminIpBlock> = get_by_desc(store, ADMIN_IP_BLOCKS, id)?;
    let Some(mut row) = existing else {
        return Ok(None);
    };
    if let Some(severity) = patch.severity {
        row.severity = severity;
    }
    if let Some(comment) = patch.comment {
        row.comment = comment.unwrap_or_default();
    }
    if let Some(expires_at_ms) = patch.expires_at_ms {
        row.expires_at_ms = expires_at_ms;
    }
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
        rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
    }
    write.commit()?;
    Ok(Some(row))
}

pub(super) fn admin_delete_ip_block(store: &RedbStore, id: Id) -> Result<Option<AdminIpBlock>> {
    let existing: Option<AdminIpBlock> = get_by_desc(store, ADMIN_IP_BLOCKS, id)?;
    let Some(row) = existing else { return Ok(None) };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
        rows.remove(schema::desc_id(id.value()))?;
    }
    {
        let mut by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
        by_ip.remove(row.ip.as_str())?;
    }
    write.commit()?;
    Ok(Some(row))
}
