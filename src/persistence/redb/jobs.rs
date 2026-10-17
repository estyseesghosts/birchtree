//! Durable job queue: enqueue, lease, retry with backoff, quarantine.
//!
//! The scheduling index is keyed by `(timestamp, id)` where the timestamp is
//! the due time for a pending job or the lease expiry for a leased one. This
//! means a job whose worker died mid-run becomes claimable again automatically
//! once its lease elapses — no recovery scan required.

use redb::{ReadableDatabase, ReadableTable, WriteTransaction};

use crate::application::ports::JobQueue;
use crate::domain::job::{backoff_ms, LEASE_MS};
use crate::domain::{Error, Id, Job, JobKind, JobState, Result};

use super::schema::{self, JOBS, JOBS_INDEX};
use super::{decode, encode, RedbStore, StoreResult};

/// Insert a job and its scheduling index entry into an existing transaction.
///
/// Used by other repositories so a mutation and its follow-up job commit
/// together.
pub(super) fn insert_job(write: &WriteTransaction, job: &Job) -> StoreResult<()> {
    {
        let mut jobs = write.open_table(JOBS)?;
        jobs.insert(job.id.value(), encode(job)?.as_slice())?;
    }
    {
        let mut index = write.open_table(JOBS_INDEX)?;
        index.insert(
            schema::job_index_key(job.due_at_ms, job.id.value()).as_slice(),
            job.id.value(),
        )?;
    }
    Ok(())
}

fn store_job(write: &WriteTransaction, job: &Job) -> StoreResult<()> {
    let mut jobs = write.open_table(JOBS)?;
    jobs.insert(job.id.value(), encode(job)?.as_slice())?;
    Ok(())
}

fn read_job_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Job>> {
    let jobs = write.open_table(JOBS)?;
    match jobs.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn remove_index(write: &WriteTransaction, timestamp_ms: u64, id: Id) -> StoreResult<()> {
    let mut index = write.open_table(JOBS_INDEX)?;
    index.remove(schema::job_index_key(timestamp_ms, id.value()).as_slice())?;
    Ok(())
}

fn add_index(write: &WriteTransaction, timestamp_ms: u64, id: Id) -> StoreResult<()> {
    let mut index = write.open_table(JOBS_INDEX)?;
    index.insert(
        schema::job_index_key(timestamp_ms, id.value()).as_slice(),
        id.value(),
    )?;
    Ok(())
}

impl RedbStore {
    /// All job records (diagnostics and tests).
    pub fn all_jobs(&self) -> Result<Vec<Job>> {
        let read = self.db().begin_read()?;
        let table = read.open_table(JOBS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_k, v) = entry?;
            out.push(decode(v.value())?);
        }
        out.sort_by_key(|j: &Job| j.id);
        Ok(out)
    }

    /// Number of entries in the scheduling index (diagnostics and tests).
    pub fn schedule_index_len(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let read = self.db().begin_read()?;
        let table = read.open_table(JOBS_INDEX)?;
        Ok(table.len()?)
    }
}

impl JobQueue for RedbStore {
    fn enqueue(&self, kind: JobKind, payload: serde_json::Value, now_ms: u64) -> Result<Job> {
        let job = Job::new(self.next_id(), kind, payload, now_ms, now_ms);
        let write = self.db().begin_write()?;
        insert_job(&write, &job)?;
        write.commit()?;
        Ok(job)
    }

    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let write = self.db().begin_write()?;

        // Collect due (timestamp, id) pairs. The upper bound (now, u64::MAX)
        // covers every key with timestamp <= now.
        let mut due: Vec<(u64, Id)> = Vec::new();
        {
            let index = write.open_table(JOBS_INDEX)?;
            let upper = schema::job_index_key(now_ms, u64::MAX);
            for entry in index.range(..=upper.as_slice())? {
                let (key, value) = entry?;
                let bytes = key.value();
                let ts = u64::from_be_bytes(bytes[..8].try_into().unwrap());
                due.push((ts, Id::new(value.value())));
                if due.len() >= limit {
                    break;
                }
            }
        }

        let mut claimed = Vec::new();
        for (index_ts, id) in due {
            let Some(mut job) = read_job_txn(&write, id)? else {
                // Orphaned index entry; drop it.
                remove_index(&write, index_ts, id)?;
                continue;
            };
            if job.state.is_terminal() {
                remove_index(&write, index_ts, id)?;
                continue;
            }
            // Move the index entry from the old timestamp to the lease expiry.
            remove_index(&write, index_ts, id)?;
            job.state = JobState::Leased;
            job.lease_until_ms = Some(now_ms + LEASE_MS);
            job.due_at_ms = now_ms + LEASE_MS;
            job.attempts = job.attempts.saturating_add(1);
            store_job(&write, &job)?;
            add_index(&write, job.due_at_ms, id)?;
            claimed.push(job);
        }

        write.commit()?;
        Ok(claimed)
    }

    fn complete(&self, id: Id) -> Result<()> {
        let write = self.db().begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        remove_index(&write, job.due_at_ms, id)?;
        job.state = JobState::Succeeded;
        job.lease_until_ms = None;
        store_job(&write, &job)?;
        write.commit()?;
        Ok(())
    }

    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job> {
        let write = self.db().begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        remove_index(&write, job.due_at_ms, id)?;
        job.lease_until_ms = None;
        job.last_error = Some(error.to_string());

        if job.attempts >= job.max_attempts {
            job.state = JobState::Quarantined;
        } else {
            job.state = JobState::Pending;
            job.due_at_ms = now_ms + backoff_ms(job.attempts);
            add_index(&write, job.due_at_ms, id)?;
        }
        store_job(&write, &job)?;
        write.commit()?;
        Ok(job)
    }

    fn quarantine(&self, id: Id, error: &str) -> Result<()> {
        let write = self.db().begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        remove_index(&write, job.due_at_ms, id)?;
        job.state = JobState::Quarantined;
        job.lease_until_ms = None;
        job.last_error = Some(error.to_string());
        store_job(&write, &job)?;
        write.commit()?;
        Ok(())
    }

    fn get(&self, id: Id) -> Result<Option<Job>> {
        let read = self.db().begin_read()?;
        let jobs = read.open_table(JOBS)?;
        match jobs.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }
}
