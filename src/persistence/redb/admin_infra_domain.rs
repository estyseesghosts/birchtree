//! Admin domain allows and domain blocks (S6 slice of `admin_infra`).
//!
//! Global ordered tables keyed by `desc_id(row_id)` (forward scan is newest
//! first) plus per-value unique indexes. One `redb` transaction per method.

use redb::ReadableTable;

use crate::application::admin_infra as use_cases;
use crate::application::ports::AdminDomainBlockPatch;
use crate::domain::{AdminDomainAllow, AdminDomainBlock, AdminDomainSeverity, Error, Id, Result};

use super::admin_scan_ctx::{get_by_desc, page_desc};
use super::schema::{
    self, ADMIN_DOMAIN_ALLOW_BY_DOMAIN, ADMIN_DOMAIN_ALLOWS, ADMIN_DOMAIN_BLOCK_BY_DOMAIN,
    ADMIN_DOMAIN_BLOCKS,
};
use super::{RedbStore, decode, encode};

// -- Domain allows --

pub(super) fn admin_create_domain_allow(
    store: &RedbStore,
    domain: &str,
    now_ms: u64,
) -> Result<AdminDomainAllow> {
    let domain = use_cases::normalize_domain(domain);
    use_cases::validate_domain(&domain)?;
    let write = store.begin_write()?;
    let existing_id: Option<u64> = {
        let by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
        by_domain.get(domain.as_str())?.map(|guard| guard.value())
    };
    if let Some(existing_id) = existing_id {
        let id = Id::new(existing_id);
        let row: Option<AdminDomainAllow> = {
            let rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
            match rows.get(schema::desc_id(id.value()))? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        if let Some(row) = row {
            return Ok(row);
        }
    }
    let row = AdminDomainAllow {
        id: store.next_id(),
        domain: domain.clone(),
        created_at_ms: now_ms,
    };
    {
        let mut rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
        by_domain.insert(domain.as_str(), row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_domain_allow_by_id(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminDomainAllow>> {
    get_by_desc(store, ADMIN_DOMAIN_ALLOWS, id)
}

pub(super) fn admin_domain_allows_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminDomainAllow>> {
    page_desc(store, ADMIN_DOMAIN_ALLOWS, max_id, since_id, min_id, limit)
}

pub(super) fn admin_delete_domain_allow(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminDomainAllow>> {
    let existing: Option<AdminDomainAllow> = get_by_desc(store, ADMIN_DOMAIN_ALLOWS, id)?;
    let Some(row) = existing else { return Ok(None) };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
        rows.remove(schema::desc_id(id.value()))?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
        by_domain.remove(row.domain.as_str())?;
    }
    write.commit()?;
    Ok(Some(row))
}

// -- Domain blocks --

pub(super) fn admin_create_domain_block(
    store: &RedbStore,
    domain: &str,
    patch: AdminDomainBlockPatch,
    now_ms: u64,
) -> Result<AdminDomainBlock> {
    let domain = use_cases::normalize_domain(domain);
    use_cases::validate_domain(&domain)?;
    let write = store.begin_write()?;
    let duplicate: bool = {
        let by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.get(domain.as_str())?.is_some()
    };
    if duplicate {
        return Err(Error::InvalidInput(
            "Validation failed: Domain has already been taken".into(),
        ));
    }
    let row = AdminDomainBlock {
        id: store.next_id(),
        domain: domain.clone(),
        severity: patch.severity.unwrap_or(AdminDomainSeverity::Silence),
        reject_media: patch.reject_media.unwrap_or(false),
        reject_reports: patch.reject_reports.unwrap_or(false),
        private_comment: patch.private_comment.unwrap_or(None),
        public_comment: patch.public_comment.unwrap_or(None),
        obfuscate: patch.obfuscate.unwrap_or(false),
        created_at_ms: now_ms,
    };
    {
        let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.insert(domain.as_str(), row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_domain_block_by_id(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminDomainBlock>> {
    get_by_desc(store, ADMIN_DOMAIN_BLOCKS, id)
}

pub(super) fn admin_domain_blocks_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminDomainBlock>> {
    page_desc(store, ADMIN_DOMAIN_BLOCKS, max_id, since_id, min_id, limit)
}

pub(super) fn admin_update_domain_block(
    store: &RedbStore,
    id: Id,
    patch: AdminDomainBlockPatch,
    _now_ms: u64,
) -> Result<Option<AdminDomainBlock>> {
    let existing: Option<AdminDomainBlock> = get_by_desc(store, ADMIN_DOMAIN_BLOCKS, id)?;
    let Some(mut row) = existing else {
        return Ok(None);
    };
    if let Some(severity) = patch.severity {
        row.severity = severity;
    }
    if let Some(reject_media) = patch.reject_media {
        row.reject_media = reject_media;
    }
    if let Some(reject_reports) = patch.reject_reports {
        row.reject_reports = reject_reports;
    }
    if let Some(private_comment) = patch.private_comment {
        row.private_comment = private_comment;
    }
    if let Some(public_comment) = patch.public_comment {
        row.public_comment = public_comment;
    }
    if let Some(obfuscate) = patch.obfuscate {
        row.obfuscate = obfuscate;
    }
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
        rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
    }
    write.commit()?;
    Ok(Some(row))
}

pub(super) fn admin_delete_domain_block(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminDomainBlock>> {
    let existing: Option<AdminDomainBlock> = get_by_desc(store, ADMIN_DOMAIN_BLOCKS, id)?;
    let Some(row) = existing else { return Ok(None) };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
        rows.remove(schema::desc_id(id.value()))?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.remove(row.domain.as_str())?;
    }
    write.commit()?;
    Ok(Some(row))
}
