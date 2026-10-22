//! Shared scan context for the admin-infrastructure slices (S6).
//!
//! Scan-cap constants and the small read helpers shared by the
//! `admin_infra_*` modules. Every admin list stays bounded: pages walk at
//! most `PAGE_SCAN_CAP` index entries, metric full-table scans at most
//! `METRIC_SCAN_CAP`, and tag-registry materialization at most
//! `TAG_SCAN_CAP`.
//!
//! One `redb` transaction per port method (`birchtree.md` §5): the helpers
//! below never open their own transaction. `bounded_scan` walks an already
//! opened read table and `audit_append` writes inside the caller's write
//! transaction, so moving call sites onto them changes no commit boundary.

use crate::domain::{AdminAuditRecord, Id, Result};

use super::schema::{self, ADMIN_AUDIT};
use super::{RedbStore, decode, encode};

/// Scan cap for admin list pages (upstream allows up to 500 per page).
pub(super) const PAGE_SCAN_CAP: usize = 5_000;

/// Scan cap for the metric full-table scans.
pub(super) const METRIC_SCAN_CAP: usize = 200_000;

/// Scan cap for distinct hashtag collection (tag registry materialization).
pub(super) const TAG_SCAN_CAP: usize = 20_000;

/// Bounded newest-first page over a `desc_id`-keyed global table.
pub(super) fn page_desc<T>(
    store: &RedbStore,
    table: redb::TableDefinition<u64, &[u8]>,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    use super::StoreError;
    if limit == 0 {
        return Ok(Vec::new());
    }
    if max_id.is_some_and(|m| m.value() == 0) {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let index: redb::ReadOnlyTable<u64, &[u8]> =
        read.open_table(table).map_err(StoreError::from)?;
    // Keys hold `desc(id)`: ascending key order is newest first.
    // `max_id` is exclusive (older than it); `since_id`/`min_id` are exclusive
    // (newer than them) — the same mapping as `notifications_page`.
    let lower = max_id
        .map(|m| schema::desc_id(m.value()).saturating_add(1))
        .unwrap_or(0);
    let upper_exclusive = since_id.or(min_id).map(|x| schema::desc_id(x.value()));
    let mut out: Vec<T> = Vec::new();
    let mut scanned = 0usize;
    // Two identical loops (rather than one boxed iterator) so the range type
    // never needs an annotation.
    if let Some(upper) = upper_exclusive {
        for entry in index.range(lower..upper)? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            out.push(decode(value.value())?);
            if out.len() >= limit {
                break;
            }
        }
    } else {
        for entry in index.range(lower..)? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            out.push(decode(value.value())?);
            if out.len() >= limit {
                break;
            }
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

pub(super) fn get_by_desc<T>(
    store: &RedbStore,
    table: redb::TableDefinition<u64, &[u8]>,
    id: Id,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    use super::StoreError;
    let read = store.begin_read()?;
    let rows: redb::ReadOnlyTable<u64, &[u8]> = read.open_table(table).map_err(StoreError::from)?;
    match rows.get(schema::desc_id(id.value()))? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Append one audit record inside the caller's write transaction.
///
/// Verbatim extraction of the insert every admin write repeats: the caller
/// keeps its single-transaction boundary (`birchtree.md` §5) and delegates
/// only the `ADMIN_AUDIT` insert here.
pub(super) fn audit_append(
    write: &redb::WriteTransaction,
    record: &AdminAuditRecord,
) -> Result<()> {
    let mut log = write.open_table(ADMIN_AUDIT)?;
    log.insert(record.id.value(), encode(record)?.as_slice())?;
    Ok(())
}
