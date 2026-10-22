//! Admin tag flags and the tag-registry materialization (S6 slice of `admin_infra`).
//!
//! Global ordered table keyed by `desc_id(row_id)` (forward scan is newest
//! first) plus the per-name unique index. One `redb` transaction per method.

use std::collections::BTreeSet;

use redb::ReadableTable;

use crate::application::ports::AdminTagPatch;
use crate::domain::{AdminTag, Error, Id, Result};

use super::admin_scan_ctx::{TAG_SCAN_CAP, get_by_desc, page_desc};
use super::schema::{self, ADMIN_TAG_BY_NAME, ADMIN_TAGS, HASHTAG_INDEX};
use super::{RedbStore, decode, encode};

// -- Tags --

pub(super) fn admin_ensure_tag(store: &RedbStore, name: &str, now_ms: u64) -> Result<AdminTag> {
    let write = store.begin_write()?;
    let existing_id: Option<u64> = {
        let by_name = write.open_table(ADMIN_TAG_BY_NAME)?;
        by_name.get(name)?.map(|guard| guard.value())
    };
    if let Some(existing_id) = existing_id {
        let id = Id::new(existing_id);
        let row: Option<AdminTag> = {
            let rows = write.open_table(ADMIN_TAGS)?;
            match rows.get(schema::desc_id(id.value()))? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        if let Some(row) = row {
            return Ok(row);
        }
    }
    let row = AdminTag {
        id: store.next_id(),
        name: name.to_string(),
        display_name: name.to_string(),
        trendable: None,
        usable: None,
        listable: None,
        reviewed_at_ms: None,
        created_at_ms: now_ms,
    };
    {
        let mut rows = write.open_table(ADMIN_TAGS)?;
        rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
    }
    {
        let mut by_name = write.open_table(ADMIN_TAG_BY_NAME)?;
        by_name.insert(name, row.id.value())?;
    }
    write.commit()?;
    Ok(row)
}

pub(super) fn admin_tags_page(
    store: &RedbStore,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<AdminTag>> {
    page_desc(store, ADMIN_TAGS, max_id, since_id, min_id, limit)
}

pub(super) fn admin_tag_by_id(store: &RedbStore, id: Id) -> Result<Option<AdminTag>> {
    get_by_desc(store, ADMIN_TAGS, id)
}

pub(super) fn admin_update_tag(
    store: &RedbStore,
    id: Id,
    patch: AdminTagPatch,
    now_ms: u64,
) -> Result<Option<AdminTag>> {
    let existing: Option<AdminTag> = get_by_desc(store, ADMIN_TAGS, id)?;
    let Some(mut row) = existing else {
        return Ok(None);
    };
    if let Some(display_name) = patch.display_name {
        if display_name.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Validation failed: Display name can't be blank".into(),
            ));
        }
        row.display_name = display_name;
    }
    if let Some(trendable) = patch.trendable {
        row.trendable = Some(trendable);
    }
    if let Some(usable) = patch.usable {
        row.usable = Some(usable);
    }
    if let Some(listable) = patch.listable {
        row.listable = Some(listable);
    }
    // Upstream stamps `reviewed_at` on every admin tag update.
    row.reviewed_at_ms = Some(now_ms);
    let write = store.begin_write()?;
    {
        let mut rows = write.open_table(ADMIN_TAGS)?;
        rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
    }
    write.commit()?;
    Ok(Some(row))
}

pub(super) fn admin_hashtag_names(store: &RedbStore, cap: usize) -> Result<Vec<String>> {
    let read = store.begin_read()?;
    let index = read.open_table(HASHTAG_INDEX)?;
    let mut tags = BTreeSet::new();
    let mut scanned = 0usize;
    for entry in index.iter()? {
        let (key, _value) = entry?;
        scanned += 1;
        if scanned > TAG_SCAN_CAP {
            break;
        }
        let bytes = key.value();
        let end = bytes.iter().position(|&b| b == 0xFF).unwrap_or(bytes.len());
        let tag = String::from_utf8_lossy(&bytes[..end]).to_string();
        tags.insert(tag);
        if tags.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(tags.into_iter().collect())
}

/// Distinct hashtag names from the hashtag index (bounded), for tag registry
/// materialization. Exported for the endpoint module: the GET index ensures a
/// row per real tag before paging, so the registry is never fabricated.
pub fn collect_hashtag_names(store: &RedbStore, cap: usize) -> Result<Vec<String>> {
    let read = store.begin_read()?;
    let index = read.open_table(HASHTAG_INDEX)?;
    let mut tags = BTreeSet::new();
    let mut scanned = 0usize;
    for entry in index.iter()? {
        let (key, _value) = entry?;
        scanned += 1;
        if scanned > TAG_SCAN_CAP {
            break;
        }
        let bytes = key.value();
        let end = bytes.iter().position(|&b| b == 0xFF).unwrap_or(bytes.len());
        let tag = String::from_utf8_lossy(&bytes[..end]).to_string();
        tags.insert(tag);
        if tags.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(tags.into_iter().collect())
}
