//! Admin email-domain blocks and canonical email blocks (S6 slice of `admin_infra`).
//!
//! Global ordered tables keyed by `desc_id(row_id)` (forward scan is newest
//! first) plus per-value unique indexes. One `redb` transaction per method.

use redb::ReadableTable;

use crate::application::admin_infra as use_cases;
use crate::domain::{AdminCanonicalEmailBlock, AdminEmailDomainBlock, Error, Id, Result};

use super::admin_scan_ctx::{PAGE_SCAN_CAP, get_by_desc, page_desc};
use super::schema::{
    self, ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH, ADMIN_CANONICAL_EMAIL_BLOCKS,
    ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN, ADMIN_EMAIL_DOMAIN_BLOCKS,
};
use super::{RedbStore, decode, encode};

// -- Email-domain blocks --

pub(super) fn admin_create_email_domain_block(
    store: &RedbStore,
    domain: &str,
    allow_with_approval: bool,
    now_ms: u64,
) -> Result<AdminEmailDomainBlock> {
    let domain = use_cases::normalize_domain(domain);
    use_cases::validate_domain(&domain)?;
    let write = store.begin_write()?;
    let duplicate: bool = {
        let by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.get(domain.as_str())?.is_some()
    };
    if duplicate {
        return Err(Error::InvalidInput(
            "Validation failed: Domain has already been taken".into(),
        ));
    }
    let row = AdminEmailDomainBlock {
        id: store.next_id(),
        domain: domain.clone(),
        allow_with_approval,
        created_at_ms: now_ms,
    };
    {
        let mut rows = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCKS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.insert(domain.as_str(), row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_email_domain_block_by_id(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminEmailDomainBlock>> {
    get_by_desc(store, ADMIN_EMAIL_DOMAIN_BLOCKS, id)
}

pub(super) fn email_domain_block_for_domain(
    store: &RedbStore,
    domain: &str,
) -> Result<Option<AdminEmailDomainBlock>> {
    // Domains are unique at creation (`admin_create_email_domain_block`
    // refuses duplicates), so the by-domain index holds at most one row.
    let normalized = use_cases::normalize_domain(domain);
    if normalized.is_empty() {
        return Ok(None);
    }
    let found: Option<u64> = {
        let read = store.begin_read()?;
        let by_domain = read.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain
            .get(normalized.as_str())?
            .map(|guard| guard.value())
    };
    let Some(id) = found else {
        return Ok(None);
    };
    get_by_desc(store, ADMIN_EMAIL_DOMAIN_BLOCKS, Id::new(id))
}

pub(super) fn admin_email_domain_blocks_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminEmailDomainBlock>> {
    page_desc(
        store,
        ADMIN_EMAIL_DOMAIN_BLOCKS,
        max_id,
        since_id,
        min_id,
        limit,
    )
}

pub(super) fn admin_delete_email_domain_block(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminEmailDomainBlock>> {
    let existing: Option<AdminEmailDomainBlock> =
        get_by_desc(store, ADMIN_EMAIL_DOMAIN_BLOCKS, id)?;
    let Some(row) = existing else { return Ok(None) };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCKS)?;
        rows.remove(schema::desc_id(id.value()))?;
    }
    {
        let mut by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
        by_domain.remove(row.domain.as_str())?;
    }
    write.commit()?;
    Ok(Some(row))
}

// -- Canonical email blocks --

pub(super) fn admin_create_canonical_email_block(
    store: &RedbStore,
    email_hash: &str,
    now_ms: u64,
) -> Result<AdminCanonicalEmailBlock> {
    if email_hash.trim().is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Canonical email hash can't be blank".into(),
        ));
    }
    let row = AdminCanonicalEmailBlock {
        id: store.next_id(),
        canonical_email_hash: email_hash.to_string(),
        created_at_ms: now_ms,
    };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_hash = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH)?;
        by_hash.insert(email_hash, row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_canonical_email_block_by_id(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminCanonicalEmailBlock>> {
    get_by_desc(store, ADMIN_CANONICAL_EMAIL_BLOCKS, id)
}

pub(super) fn admin_canonical_email_blocks_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminCanonicalEmailBlock>> {
    page_desc(
        store,
        ADMIN_CANONICAL_EMAIL_BLOCKS,
        max_id,
        since_id,
        min_id,
        limit,
    )
}

pub(super) fn admin_canonical_blocks_for_hash(
    store: &RedbStore,
    email_hash: &str,
) -> Result<Vec<AdminCanonicalEmailBlock>> {
    if email_hash.is_empty() {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let rows = read.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
    let mut out = Vec::new();
    let mut scanned = 0usize;
    for entry in rows.iter()? {
        let (_key, value) = entry?;
        scanned += 1;
        if scanned > PAGE_SCAN_CAP {
            break;
        }
        let row: AdminCanonicalEmailBlock = decode(value.value())?;
        if row.canonical_email_hash == email_hash {
            out.push(row);
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn admin_delete_canonical_email_block(
    store: &RedbStore,
    id: Id,
) -> Result<Option<AdminCanonicalEmailBlock>> {
    let existing: Option<AdminCanonicalEmailBlock> =
        get_by_desc(store, ADMIN_CANONICAL_EMAIL_BLOCKS, id)?;
    let Some(row) = existing else { return Ok(None) };
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
        rows.remove(schema::desc_id(id.value()))?;
    }
    {
        let mut by_hash = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH)?;
        by_hash.remove(row.canonical_email_hash.as_str())?;
    }
    write.commit()?;
    Ok(Some(row))
}
