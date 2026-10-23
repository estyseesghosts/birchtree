//! Status row access and read queries.
//!
//! Shared transaction helpers (`read_status_txn`, `write_status_txn`,
//! `ensure_id_unused`) plus the read surface: `status_by_id`,
//! `statuses_by_ids`, `own_reblog`, `status_children`, `status_versions`,
//! `statuses_by_account`, `viewer_status_flags` and the tag-backfill scanner.
//!
//! Every other `statuses_*` module builds on the helpers here. External
//! persistence modules (`quotes`, `federation_content`, `media`, `polls`,
//! `notifications`) keep working through re-exports the parent adds when the
//! old `statuses.rs` is deleted.

use std::collections::HashMap;

use redb::{ReadTransaction, ReadableTable, WriteTransaction};

use crate::application::ports::{AccountStatusesQuery, StatusViewFlags};
use crate::domain::{Id, Result, Status, StatusVersion};

use super::schema::{
    self, BOOKMARKS, FAVOURITES, REBLOGS_BY_ACCOUNT, STATUS_AUTHOR_INDEX, STATUS_PARENT_INDEX,
    STATUS_PINS, STATUS_VERSIONS, STATUSES,
};
use super::{RedbStore, StoreResult, decode, encode};

/// Bounded scan of a status's reply-children index.
pub(super) const CHILDREN_SCAN_CAP: usize = 1_000;
/// Bounded scan when tombstoning the reblogs / quotes of a status.
pub(super) const CASCADE_SCAN_CAP: usize = 1_000;
/// Bounded scan when recomputing an account's `last_status_at`.
pub(super) const LAST_STATUS_SCAN_CAP: usize = 200;
/// Bounded scan over one account's author index for timeline queries.
pub(super) const AUTHOR_SCAN_CAP: usize = 2_000;
/// Bounded scan of an account's pins (the API caps pins at 5 anyway).
pub(super) const PINS_SCAN_CAP: usize = 100;
/// Bounded scan of `favourited_by` / `reblogged_by` reverse indexes.
pub(super) const INTERACTION_SCAN_CAP: usize = 500;

/// Tables keyed by a generated row ID (`u64`, raw, not flipped), scanned to
/// seed the ID generator above everything persisted.
const GENERATED_ID_TABLES: &[redb::TableDefinition<'static, u64, &'static [u8]>] = &[
    schema::ACCOUNTS,
    schema::JOBS,
    schema::STATUSES,
    schema::ANNOUNCEMENTS,
    schema::ANNOUNCEMENT_REACTIONS,
    schema::POLLS,
    schema::QUOTES,
    schema::NOTIFICATIONS,
    schema::NOTIFICATION_REQUESTS,
    schema::MEDIA,
    schema::PUSH_SUBSCRIPTIONS,
    schema::LISTS,
    schema::FILTERS,
    schema::FILTER_KEYWORDS,
    schema::FILTER_STATUSES,
    schema::SCHEDULED_STATUSES,
    schema::REPORTS,
    schema::ADMIN_AUDIT,
    schema::RULES,
    schema::ACCOUNT_WARNINGS,
    schema::SIGN_INS,
    schema::INTERACTION_EVENTS,
    schema::ADMIN_DOMAIN_ALLOWS,
    schema::ADMIN_DOMAIN_BLOCKS,
    schema::ADMIN_EMAIL_DOMAIN_BLOCKS,
    schema::ADMIN_IP_BLOCKS,
    schema::ADMIN_CANONICAL_EMAIL_BLOCKS,
    schema::ADMIN_TAGS,
    schema::TREND_LINKS,
    schema::TREND_PUBLISHERS,
    schema::ANNUAL_REPORTS,
    schema::COLLECTIONS,
];

impl RedbStore {
    /// Whether a write transaction can be opened (and rolled back) now.
    ///
    /// Readiness uses this to prove write capability without changing data.
    /// It waits for any in-flight writer, so callers run it off the async
    /// threads under a deadline.
    pub fn can_write(&self) -> bool {
        match self.db.begin_write() {
            Ok(write) => write.abort().is_ok(),
            Err(_) => false,
        }
    }

    /// The highest generated row ID persisted in any ID-keyed primary table
    /// (zero for an empty database). Startup passes it to
    /// [`IdGenerator::observe`](crate::domain::IdGenerator::observe) before
    /// serving, so a restart, clock rollback or restored backup cannot mint an
    /// ID at or below an existing row.
    pub fn highest_persisted_id(&self) -> StoreResult<Id> {
        use redb::ReadableDatabase;

        let read = self.db.begin_read()?;
        let mut highest: u64 = 0;
        for definition in GENERATED_ID_TABLES {
            let table = match read.open_table(*definition) {
                Ok(table) => table,
                Err(redb::TableError::TableDoesNotExist(_)) => continue,
                Err(err) => return Err(err.into()),
            };
            if let Some(entry) = table.last()? {
                highest = highest.max(entry.0.value());
            }
        }
        Ok(Id::new(highest))
    }
}

/// Refuse to create a row whose generated ID already exists in `definition`.
///
/// Primary-record creates call this before inserting so an ID collision
/// (restored backup, rolled-back clock, bug) fails loudly with a storage
/// error instead of silently replacing an existing row. Upserts of rows that must
/// already exist do not use it.
pub(super) fn ensure_id_unused(
    write: &WriteTransaction,
    definition: redb::TableDefinition<'static, u64, &'static [u8]>,
    id: Id,
) -> StoreResult<()> {
    let table = write.open_table(definition)?;
    if table.get(id.value())?.is_some() {
        let collision = std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("duplicate generated id {id}"),
        );
        return Err(collision.into());
    }
    Ok(())
}

/// Read a status row inside an open write transaction.
pub(super) fn read_status_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Status>> {
    let table = write.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Write (upsert) a status row — and its author-index entry, which is keyed
/// from immutable fields — inside an open write transaction.
pub(super) fn write_status_txn(write: &WriteTransaction, status: &Status) -> StoreResult<()> {
    {
        let mut table = write.open_table(STATUSES)?;
        table.insert(status.id.value(), encode(status)?.as_slice())?;
    }
    {
        let mut by_uri = write.open_table(schema::STATUS_BY_URI)?;
        by_uri.insert(status.uri.as_str(), status.id.value())?;
    }
    {
        let mut table = write.open_table(STATUS_AUTHOR_INDEX)?;
        table.insert(
            schema::pair_key(
                status.account_id.value(),
                schema::desc_id(status.id.value()),
            )
            .as_slice(),
            status.id.value(),
        )?;
    }
    maintain_timeline_index(write, status)?;
    Ok(())
}

/// Keep the public/local timeline ranges in step with a status write.
///
/// A status joins `TIMELINE_PUBLIC` when it is public and not deleted, and
/// `TIMELINE_LOCAL` when its author's account row is local (localness comes
/// from the author account inside the same transaction). Tombstones and
/// non-public visibilities remove the entries; every step is idempotent, so
/// the writer can be called for counter bumps and reblogs alike.
pub(super) fn maintain_timeline_index(
    write: &WriteTransaction,
    status: &Status,
) -> StoreResult<()> {
    use crate::domain::Visibility;

    let desc = schema::desc_id(status.id.value());
    {
        let mut public = write.open_table(schema::TIMELINE_PUBLIC)?;
        let mut local = write.open_table(schema::TIMELINE_LOCAL)?;
        if status.is_deleted() || status.visibility != Visibility::Public {
            public.remove(desc)?;
            local.remove(desc)?;
            return Ok(());
        }
        public.insert(desc, ())?;
        // Author locality, read inside this transaction (the account row is
        // small and always present for local statuses).
        let accounts = write.open_table(schema::ACCOUNTS)?;
        let author_local = match accounts.get(status.account_id.value())? {
            Some(guard) => decode::<crate::domain::Account>(guard.value())?.local,
            None => false,
        };
        if author_local {
            local.insert(desc, ())?;
        } else {
            local.remove(desc)?;
        }
    }
    Ok(())
}

/// Read a status row inside an open read transaction.
pub(super) fn status_at(read: &ReadTransaction, id: Id) -> StoreResult<Option<Status>> {
    let table = read.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Fetch one status by id.
pub(super) fn status_by_id_impl(store: &RedbStore, id: Id) -> Result<Option<Status>> {
    let read = store.begin_read()?;
    Ok(status_at(&read, id)?)
}

/// Fetch non-deleted statuses among `ids`, ordered by id descending.
pub(super) fn statuses_by_ids_impl(store: &RedbStore, ids: &[Id]) -> Result<Vec<Status>> {
    let read = store.begin_read()?;
    let table = read.open_table(STATUSES)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(guard) = table.get(id.value())? else {
            continue;
        };
        let status: Status = decode(guard.value())?;
        if !status.is_deleted() {
            out.push(status);
        }
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(out)
}

/// The live reblog wrapper `account_id` created of `status_id`, if any.
pub(super) fn own_reblog_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
) -> Result<Option<Status>> {
    let read = store.begin_read()?;
    let reblog_id = {
        let table = read.open_table(REBLOGS_BY_ACCOUNT)?;
        let key = schema::pair_key(account_id.value(), status_id.value());
        table
            .get(key.as_slice())?
            .map(|guard| Id::new(guard.value()))
    };
    let Some(reblog_id) = reblog_id else {
        return Ok(None);
    };
    Ok(status_at(&read, reblog_id)?.filter(|s| !s.is_deleted()))
}

/// Direct reply children of `parent_id`, newest first, cursor-bounded.
pub(super) fn status_children_impl(
    store: &RedbStore,
    parent_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Status>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;

    // Keys are `(parent || child)` ascending; collect (bounded), then take
    // the newest `limit` below the cursor.
    let mut ids: Vec<Id> = Vec::new();
    {
        let index = read.open_table(STATUS_PARENT_INDEX)?;
        let lo = schema::pair_key(parent_id.value(), 0);
        let hi = schema::pair_key(parent_id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            ids.push(Id::new(value.value()));
            if ids.len() >= CHILDREN_SCAN_CAP {
                break;
            }
        }
        store.note_scanned(ids.len());
    }
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let selected: Vec<Id> = ids
        .into_iter()
        .filter(|cid| max_id.is_none_or(|m| *cid < m))
        .take(limit)
        .collect();

    let mut out = Vec::with_capacity(selected.len());
    let statuses = read.open_table(STATUSES)?;
    for child_id in selected {
        let Some(guard) = statuses.get(child_id.value())? else {
            continue;
        };
        let status: Status = decode(guard.value())?;
        if !status.is_deleted() {
            out.push(status);
        }
    }
    Ok(out)
}

/// Edit-history snapshots, oldest first (empty when never edited).
pub(super) fn status_versions_impl(store: &RedbStore, status_id: Id) -> Result<Vec<StatusVersion>> {
    let read = store.begin_read()?;
    let table = read.open_table(STATUS_VERSIONS)?;
    let lo = schema::pair_key(status_id.value(), 0);
    let hi = schema::pair_key(status_id.value(), u64::MAX);
    let mut out: Vec<StatusVersion> = Vec::new();
    for entry in table.range(lo.as_slice()..=hi.as_slice())? {
        let (_key, value) = entry?;
        out.push(decode(value.value())?);
    }
    out.sort_by_key(|v| v.version);
    Ok(out)
}

/// Statuses authored by `account_id` (non-deleted), id-descending with
/// cursors; `pinned_only` returns the account's pins in pin order.
pub(super) fn statuses_by_account_impl(
    store: &RedbStore,
    account_id: Id,
    query: AccountStatusesQuery,
) -> Result<Vec<Status>> {
    if query.limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;

    if query.pinned_only {
        // Pin records first (pin order, newest pin first), then rows.
        return super::statuses_read_stages::pinned_statuses(&read, account_id, &query);
    }

    super::statuses_read_stages::general_statuses(&read, store, account_id, &query)
}

/// Viewer flags for a batch of statuses (no N+1).
pub(super) fn viewer_status_flags_impl(
    store: &RedbStore,
    viewer_id: Id,
    status_ids: &[Id],
) -> Result<HashMap<Id, StatusViewFlags>> {
    let read = store.begin_read()?;
    let favourites = read.open_table(FAVOURITES)?;
    let bookmarks = read.open_table(BOOKMARKS)?;
    let reblogs = read.open_table(REBLOGS_BY_ACCOUNT)?;
    let pins = read.open_table(STATUS_PINS)?;

    let mut out = HashMap::with_capacity(status_ids.len());
    for id in status_ids {
        let flags = StatusViewFlags {
            favourited: favourites
                .get(schema::pair_key(viewer_id.value(), id.value()).as_slice())?
                .is_some(),
            bookmarked: bookmarks
                .get(schema::pair_key(viewer_id.value(), id.value()).as_slice())?
                .is_some(),
            reblogged: reblogs
                .get(schema::pair_key(viewer_id.value(), id.value()).as_slice())?
                .is_some(),
            pinned: pins
                .get(schema::pair_key(viewer_id.value(), id.value()).as_slice())?
                .is_some(),
            // Conversation mutes ride the status-view map (Phase 7); the
            // port total stays neutral here and the map overrides per page.
            muted: false,
        };
        out.insert(*id, flags);
    }
    Ok(out)
}

/// One bounded page of the tag-normalization backfill (D25): newest
/// first, `(status id, stored tags, source text)`. `after` is the
/// exclusive resume cursor (`None` starts at the newest row).
pub(super) fn backfill_tag_batch_impl(
    store: &RedbStore,
    after: Option<Id>,
    cap: usize,
) -> Result<Vec<(Id, Vec<String>, String)>> {
    let cap = cap.clamp(1, 2_000);
    let read = store.begin_read()?;
    let table = read.open_table(STATUSES)?;
    // Newest first: descending scan, exclusive of the resume cursor.
    let mut out = Vec::new();
    for entry in table.iter()?.rev() {
        let (key, value) = entry?;
        let id = Id::new(key.value());
        if after.is_some_and(|cursor| id >= cursor) {
            continue;
        }
        let status: Status = decode(value.value())?;
        if status.is_deleted() {
            continue;
        }
        out.push((id, status.tags.clone(), status.source_text.clone()));
        if out.len() >= cap {
            break;
        }
    }
    Ok(out)
}
