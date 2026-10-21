//! `redb` collections rows (gaps batch, `docs/gaps.md`, G-C).
//!
//! Implements [`CollectionStore`](crate::application::collections::CollectionStore)
//! over the `COLLECTIONS` (`u64` → JSON `CollectionRecord`) and
//! `COLLECTION_ITEMS` (16-byte `(collection_id_be || status_id_be)` → status
//! id bytes) tables (schema v18).
//!
//! Every mutation commits the record, its membership rows, the denormalized
//! `items_count` and the bounded `CollectionFanout` job in ONE `redb`
//! transaction (`birchtree.md` §5): a committed mutation can never lose its
//! fan-out. Payloads carry at most 500 item ids (< 64 KiB, no blobs).
//! Notification creation and delivery happen after commit, in the job
//! handler (`application::collections::handle_fanout`).

use redb::{ReadableTable, WriteTransaction};

use crate::application::collections::{
    COLLECTION_FANOUT_BATCH, COLLECTION_MAX_ITEMS, fanout_payload,
};
use crate::application::ports::{CollectionChange, CollectionRecord, CollectionStore};
use crate::domain::{Error, Id, Job, JobKind, Result};

use super::schema::{self, COLLECTIONS, COLLECTION_ITEMS, STATUSES, STATUS_REBLOG_INDEX};
use super::{decode, encode, RedbStore, StoreResult};

/// Bound for per-collection membership scans (the item cap plus headroom, so
/// a revoke/delete pass always converges in one transaction).
const ITEM_SCAN_CAP: usize = 1_500;
/// Bound for the account-collection scan (the per-account cap is 50; the
/// scan has no by-account index, so it walks the whole table with a cap).
const COLLECTION_SCAN_CAP: usize = 20_000;
/// Bound for the distributable-reblog scan (mirrors the interaction scans).
const REBLOG_SCAN_CAP: usize = 2_000;

/// Read one collection row inside an open write transaction.
fn read_collection_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<CollectionRecord>> {
    let table = write.open_table(COLLECTIONS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// All member status ids of `collection_id` inside an open write
/// transaction, ascending by status id, bounded by [`ITEM_SCAN_CAP`].
fn member_ids_txn(write: &WriteTransaction, collection_id: Id) -> StoreResult<Vec<Id>> {
    let table = write.open_table(COLLECTION_ITEMS)?;
    let lo = schema::collection_item_key(collection_id.value(), 0);
    let hi = schema::collection_item_key(collection_id.value(), u64::MAX);
    let mut ids = Vec::new();
    for entry in table.range(lo.as_slice()..=hi.as_slice())? {
        let (key, _value) = entry?;
        ids.push(Id::new(u64::from_be_bytes(
            key.value()[8..].try_into().unwrap(),
        )));
        if ids.len() >= ITEM_SCAN_CAP {
            break;
        }
    }
    Ok(ids)
}

/// Enqueue the fan-out job for a collection change inside the caller's
/// transaction (one txn commits mutation + job, `birchtree.md` §5).
fn fanout_txn(
    write: &WriteTransaction,
    job_id: Id,
    collection_id: Id,
    change: CollectionChange,
    item_ids: &[Id],
    actor_id: Id,
    now_ms: u64,
) -> StoreResult<()> {
    let job = Job::new(
        job_id,
        JobKind::CollectionFanout,
        fanout_payload(collection_id, change, item_ids, actor_id),
        now_ms,
        now_ms,
    );
    super::jobs::insert_job(write, &job)
}

impl CollectionStore for RedbStore {
    fn create_collection(
        &self,
        account_id: Id,
        title: &str,
        description: &str,
        public: bool,
        now_ms: u64,
    ) -> Result<CollectionRecord> {
        let write = self.begin_write()?;
        let record = CollectionRecord {
            id: self.next_id(),
            account_id,
            title: title.to_string(),
            description: description.to_string(),
            public,
            revoked: false,
            items_count: 0,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(COLLECTIONS)?;
            table.insert(record.id.value(), encode(&record)?.as_slice())?;
        }
        // No fan-out: a new collection has no members to notify.
        write.commit()?;
        Ok(record)
    }

    fn collection_by_id(&self, id: Id) -> Result<Option<CollectionRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(COLLECTIONS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn collections_by_account(&self, account_id: Id) -> Result<Vec<CollectionRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(COLLECTIONS)?;
        let mut out = Vec::new();
        // No by-account index (schema v18 Step 0); bounded full scan.
        let mut walked = 0usize;
        for entry in table.iter()? {
            let (_key, value) = entry?;
            walked += 1;
            if walked > COLLECTION_SCAN_CAP {
                break;
            }
            let record: CollectionRecord = decode(value.value())?;
            if record.account_id == account_id {
                out.push(record);
            }
        }
        self.note_scanned(walked);
        // Descending collection id (newest first), like lists.
        out.sort_unstable_by(|a, b| b.id.cmp(&a.id));
        Ok(out)
    }

    fn update_collection(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        description: Option<String>,
        public: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>> {
        let write = self.begin_write()?;
        let Some(mut record) = read_collection_txn(&write, id)? else {
            return Ok(None);
        };
        if record.account_id != account_id {
            return Ok(None);
        }
        if let Some(title) = title {
            record.title = title;
        }
        if let Some(description) = description {
            record.description = description;
        }
        if let Some(public) = public {
            record.public = public;
        }
        record.updated_at_ms = now_ms;
        {
            let mut table = write.open_table(COLLECTIONS)?;
            table.insert(record.id.value(), encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(record))
    }

    fn delete_collection(&self, account_id: Id, id: Id, now_ms: u64) -> Result<bool> {
        let write = self.begin_write()?;
        let Some(record) = read_collection_txn(&write, id)? else {
            return Ok(false);
        };
        if record.account_id != account_id {
            return Ok(false);
        }
        let members = member_ids_txn(&write, id)?;
        for member in &members {
            let key = schema::collection_item_key(id.value(), member.value());
            let mut table = write.open_table(COLLECTION_ITEMS)?;
            table.remove(key.as_slice())?;
        }
        {
            let mut table = write.open_table(COLLECTIONS)?;
            table.remove(id.value())?;
        }
        fanout_txn(
            &write,
            self.next_id(),
            id,
            CollectionChange::Delete,
            &members.into_iter().take(COLLECTION_FANOUT_BATCH).collect::<Vec<_>>(),
            account_id,
            now_ms,
        )?;
        write.commit()?;
        Ok(true)
    }

    fn collection_items(
        &self,
        collection_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Id>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(COLLECTION_ITEMS)?;
        let lo = schema::collection_item_key(collection_id.value(), 0);
        let hi = schema::collection_item_key(collection_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(
                key.value()[8..].try_into().unwrap(),
            )));
            if ids.len() >= ITEM_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(ids.len());
        // Descending status id (newest first), cursor-bounded.
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids
            .into_iter()
            .filter(|id| max_id.is_none_or(|max| *id < max))
            .take(limit)
            .collect())
    }

    fn collections_containing_status(
        &self,
        account_id: Id,
        status_id: Id,
    ) -> Result<Vec<CollectionRecord>> {
        let read = self.begin_read()?;
        let collections = read.open_table(COLLECTIONS)?;
        let items = read.open_table(COLLECTION_ITEMS)?;
        // The owner's collections are capped (50), so this is at most 50
        // point gets — no unbounded scan.
        let mut owned: Vec<CollectionRecord> = Vec::new();
        let mut walked = 0usize;
        for entry in collections.iter()? {
            let (_key, value) = entry?;
            walked += 1;
            if walked > COLLECTION_SCAN_CAP {
                break;
            }
            let record: CollectionRecord = decode(value.value())?;
            if record.account_id == account_id {
                owned.push(record);
            }
        }
        self.note_scanned(walked);
        owned.sort_unstable_by(|a, b| b.id.cmp(&a.id));
        let mut out = Vec::new();
        for record in owned {
            let key = schema::collection_item_key(record.id.value(), status_id.value());
            if items.get(key.as_slice())?.is_some() {
                out.push(record);
            }
        }
        Ok(out)
    }

    fn add_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>> {
        let write = self.begin_write()?;
        let Some(mut record) = read_collection_txn(&write, collection_id)? else {
            return Err(Error::NotFound("Record not found".into()));
        };
        if record.account_id != account_id {
            return Err(Error::NotFound("Record not found".into()));
        }
        if record.revoked {
            return Err(Error::InvalidInput(
                "Validation failed: Collection has been revoked".into(),
            ));
        }
        if status_ids.len() > crate::application::collections::COLLECTION_MUTATION_BATCH_LIMIT {
            return Err(Error::InvalidInput(
                "Validation failed: Too many status ids (maximum is 500)".into(),
            ));
        }
        let existing = member_ids_txn(&write, collection_id)?;
        if existing.len() + status_ids.len() > COLLECTION_MAX_ITEMS {
            return Err(Error::InvalidInput(
                "Validation failed: Collection has too many items".into(),
            ));
        }
        let mut added = Vec::new();
        for status_id in status_ids {
            let key = schema::collection_item_key(collection_id.value(), status_id.value());
            let present = {
                let table = write.open_table(COLLECTION_ITEMS)?;
                table.get(key.as_slice())?.is_some()
            };
            if present {
                continue;
            }
            let id_bytes = status_id.value().to_be_bytes();
            {
                let mut table = write.open_table(COLLECTION_ITEMS)?;
                table.insert(key.as_slice(), id_bytes.as_slice())?;
            }
            added.push(*status_id);
        }
        if !added.is_empty() {
            record.items_count = record.items_count.saturating_add(added.len() as u64);
            record.updated_at_ms = now_ms;
            {
                let mut table = write.open_table(COLLECTIONS)?;
                table.insert(record.id.value(), encode(&record)?.as_slice())?;
            }
            fanout_txn(
                &write,
                self.next_id(),
                collection_id,
                CollectionChange::Add,
                &added,
                account_id,
                now_ms,
            )?;
        }
        write.commit()?;
        Ok(added)
    }

    fn remove_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>> {
        let write = self.begin_write()?;
        let Some(mut record) = read_collection_txn(&write, collection_id)? else {
            return Err(Error::NotFound("Record not found".into()));
        };
        if record.account_id != account_id {
            return Err(Error::NotFound("Record not found".into()));
        }
        let mut removed = Vec::new();
        for status_id in status_ids {
            let key = schema::collection_item_key(collection_id.value(), status_id.value());
            let existed = {
                let mut table = write.open_table(COLLECTION_ITEMS)?;
                table.remove(key.as_slice())?.is_some()
            };
            if existed {
                removed.push(*status_id);
            }
        }
        if !removed.is_empty() {
            record.items_count = record.items_count.saturating_sub(removed.len() as u64);
            record.updated_at_ms = now_ms;
            {
                let mut table = write.open_table(COLLECTIONS)?;
                table.insert(record.id.value(), encode(&record)?.as_slice())?;
            }
            fanout_txn(
                &write,
                self.next_id(),
                collection_id,
                CollectionChange::Remove,
                &removed,
                account_id,
                now_ms,
            )?;
        }
        write.commit()?;
        Ok(removed)
    }

    fn revoke_collection(
        &self,
        account_id: Id,
        id: Id,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>> {
        let write = self.begin_write()?;
        let Some(mut record) = read_collection_txn(&write, id)? else {
            return Ok(None);
        };
        if record.account_id != account_id {
            return Ok(None);
        }
        let members = member_ids_txn(&write, id)?;
        for member in &members {
            let key = schema::collection_item_key(id.value(), member.value());
            let mut table = write.open_table(COLLECTION_ITEMS)?;
            table.remove(key.as_slice())?;
        }
        record.items_count = 0;
        record.revoked = true;
        record.updated_at_ms = now_ms;
        {
            let mut table = write.open_table(COLLECTIONS)?;
            table.insert(record.id.value(), encode(&record)?.as_slice())?;
        }
        fanout_txn(
            &write,
            self.next_id(),
            id,
            CollectionChange::Revoke,
            &members.into_iter().take(COLLECTION_FANOUT_BATCH).collect::<Vec<_>>(),
            account_id,
            now_ms,
        )?;
        write.commit()?;
        Ok(Some(record))
    }

    fn reblogged_by_distributable(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        // `(status || reblog_id)` ascending → collect (bounded), reverse —
        // the same scan as the plain `reblogged_by`, plus the
        // distributability filter (D52).
        let reblog_ids: Vec<Id> = {
            let index = read.open_table(STATUS_REBLOG_INDEX)?;
            let lo = schema::pair_key(status_id.value(), 0);
            let hi = schema::pair_key(status_id.value(), u64::MAX);
            let mut ids = Vec::new();
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, value) = entry?;
                ids.push(Id::new(value.value()));
                if ids.len() >= REBLOG_SCAN_CAP {
                    break;
                }
            }
            ids
        };
        self.note_scanned(reblog_ids.len());

        let mut out = Vec::new();
        let statuses = read.open_table(STATUSES)?;
        for reblog_id in reblog_ids.into_iter().rev() {
            if max_id.is_some_and(|m| reblog_id >= m) {
                continue;
            }
            if out.len() >= limit {
                break;
            }
            let Some(guard) = statuses.get(reblog_id.value())? else {
                continue;
            };
            let reblog: crate::domain::Status = decode(guard.value())?;
            if reblog.is_deleted() {
                continue;
            }
            // D52 scope: only distributable (public/unlisted) reblog
            // wrappers attribute the reblogger. Upstream's
            // `distributable_visibility` scope on the store scan.
            if !reblog.visibility.is_distributable() {
                continue;
            }
            out.push((reblog.account_id, reblog_id));
        }
        Ok(out)
    }
}
