//! Scheduled status persistence.
//!
//! One transaction commits the row, its account index and the
//! `PublishScheduledStatus` job; publishing replays the stored params through
//! the normal creation use case (`birchtree.md` §5).

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewScheduledStatus, ScheduledStore};
use crate::domain::{Id, Job, JobKind, Result, ScheduledStatus};

use super::schema::{self, SCHEDULED_BY_ACCOUNT, SCHEDULED_STATUSES};
use super::{decode, encode, RedbStore, StoreResult};

/// Scan caps (Mastodon caps scheduled statuses per account at 300).
const ACCOUNT_SCAN_CAP: usize = 320;
const TABLE_SCAN_CAP: usize = 1_000;

/// Read one scheduled row inside an open transaction.
fn read_scheduled_txn(
    write: &WriteTransaction,
    id: Id,
) -> StoreResult<Option<ScheduledStatus>> {
    let table = write.open_table(SCHEDULED_STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

impl ScheduledStore for RedbStore {
    fn create_scheduled(&self, input: NewScheduledStatus, now_ms: u64) -> Result<ScheduledStatus> {
        // ONE transaction: row + account index + publish job.
        let write = self.begin_write()?;
        let row = ScheduledStatus {
            id: self.next_id(),
            account_id: input.account_id,
            scheduled_at_ms: input.scheduled_at_ms,
            media_ids: input.media_ids,
            params: input.params,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(SCHEDULED_STATUSES)?;
            table.insert(row.id.value(), encode(&row)?.as_slice())?;
        }
        {
            let mut table = write.open_table(SCHEDULED_BY_ACCOUNT)?;
            table.insert(
                schema::pair_key(row.account_id.value(), schema::desc_id(row.id.value())).as_slice(),
                (),
            )?;
        }
        let job = Job::new(
            row.id,
            JobKind::PublishScheduledStatus,
            serde_json::json!({
                "scheduled_id": row.id.value(),
                "base_url": input.base_url,
            }),
            row.scheduled_at_ms,
            now_ms,
        );
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(row)
    }

    fn scheduled_by_id(&self, id: Id) -> Result<Option<ScheduledStatus>> {
        let read = self.begin_read()?;
        let table = read.open_table(SCHEDULED_STATUSES)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn scheduled_by_account(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<ScheduledStatus>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let index = read.open_table(SCHEDULED_BY_ACCOUNT)?;
        let rows = read.open_table(SCHEDULED_STATUSES)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        self.note_scanned(0);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            let desc = u64::from_be_bytes(key.value()[8..].try_into().unwrap());
            ids.push(Id::new(u64::MAX - desc));
            if ids.len() >= ACCOUNT_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(ids.len());
        // Index order is id-descending already; apply exclusive cursors.
        let want_oldest = min_id.is_some() && since_id.is_none();
        let mut matched: Vec<ScheduledStatus> = Vec::new();
        for id in ids {
            if max_id.is_some_and(|m| id >= m)
                || since_id.is_some_and(|s| id <= s)
                || min_id.is_some_and(|m| id <= m)
            {
                continue;
            }
            let Some(guard) = rows.get(id.value())? else {
                continue;
            };
            matched.push(decode(guard.value())?);
            if !want_oldest && matched.len() >= limit {
                break;
            }
        }
        if want_oldest && matched.len() > limit {
            matched.drain(..matched.len() - limit);
        }
        Ok(matched)
    }

    fn reschedule(
        &self,
        account_id: Id,
        id: Id,
        scheduled_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ScheduledStatus>> {
        let write = self.begin_write()?;
        let Some(mut row) = read_scheduled_txn(&write, id)? else {
            return Ok(None);
        };
        if row.account_id != account_id {
            return Ok(None);
        }
        // Only `scheduled_at` is editable; the stored params and media stay.
        let _ = now_ms;
        row.scheduled_at_ms = scheduled_at_ms;
        {
            let mut table = write.open_table(SCHEDULED_STATUSES)?;
            table.insert(row.id.value(), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    fn delete_scheduled(&self, account_id: Id, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let Some(row) = read_scheduled_txn(&write, id)? else {
            return Ok(false);
        };
        if row.account_id != account_id {
            return Ok(false);
        }
        {
            let mut table = write.open_table(SCHEDULED_STATUSES)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(SCHEDULED_BY_ACCOUNT)?;
            table.remove(
                schema::pair_key(row.account_id.value(), schema::desc_id(id.value())).as_slice(),
            )?;
        }
        write.commit()?;
        Ok(true)
    }

    fn due_scheduled(&self, now_ms: u64, cap: usize) -> Result<Vec<ScheduledStatus>> {
        let read = self.begin_read()?;
        let table = read.open_table(SCHEDULED_STATUSES)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in table.range(0u64..=u64::MAX)? {
            let (_key, value) = entry?;
            scanned += 1;
            let row: ScheduledStatus = decode(value.value())?;
            if row.scheduled_at_ms <= now_ms {
                out.push(row);
                if out.len() >= cap {
                    break;
                }
            }
            if scanned >= TABLE_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn scheduled_counts(
        &self,
        account_id: Id,
        day_start_ms: u64,
        day_end_ms: u64,
    ) -> Result<(usize, usize)> {
        let read = self.begin_read()?;
        let index = read.open_table(SCHEDULED_BY_ACCOUNT)?;
        let rows = read.open_table(SCHEDULED_STATUSES)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut total = 0usize;
        let mut today = 0usize;
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            let desc = u64::from_be_bytes(key.value()[8..].try_into().unwrap());
            let id = Id::new(u64::MAX - desc);
            total += 1;
            if let Some(guard) = rows.get(id.value())? {
                let row: ScheduledStatus = decode(guard.value())?;
                if row.scheduled_at_ms >= day_start_ms && row.scheduled_at_ms < day_end_ms {
                    today += 1;
                }
            }
            if total >= ScheduledStatus::TOTAL_LIMIT + 1 {
                break;
            }
        }
        Ok((total, today))
    }
}
