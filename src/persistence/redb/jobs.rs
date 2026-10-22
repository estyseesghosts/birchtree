//! Durable job queue: enqueue, lease, retry with backoff, quarantine.
//!
//! The scheduling index is keyed by `(timestamp, id)` where the timestamp is
//! the due time for a pending job or the lease expiry for a leased one. This
//! means a job whose worker died mid-run becomes claimable again automatically
//! once its lease elapses — no recovery scan required.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::JobQueue;
use crate::domain::job::{LEASE_MS, backoff_ms};
use crate::domain::{Error, Id, Job, JobKind, JobState, Result};

use super::schema::{self, DELIVERY_ACTIVE, JOBS, JOBS_INDEX};
use super::{RedbStore, StoreResult, decode, encode};

/// Whether a job kind participates in the per-host FIFO gate.
fn is_delivery_kind(kind: JobKind) -> bool {
    matches!(kind, JobKind::DeliverActivity | JobKind::RelayForward)
}

/// The destination host of a delivery job, if it carries one.
fn delivery_host_of(job: &Job) -> Option<String> {
    let inbox = job
        .payload
        .get("inbox")
        .or_else(|| job.payload.get("relay_inbox"))
        .and_then(|v| v.as_str())?;
    Some(super::federation::delivery_host_key(inbox))
}

/// The stamped sequence of a delivery job (0 when absent, i.e. pre-v23 rows).
fn delivery_seq_of(job: &Job) -> u64 {
    job.payload.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
}

fn insert_delivery_active(
    write: &WriteTransaction,
    host: &str,
    seq: u64,
    job_id: Id,
) -> StoreResult<()> {
    let mut active = write.open_table(DELIVERY_ACTIVE)?;
    let key = schema::delivery_active_key(host, seq, job_id.value());
    active.insert(key.as_slice(), job_id.value())?;
    Ok(())
}

fn remove_delivery_active(
    write: &WriteTransaction,
    host: &str,
    seq: u64,
    job_id: Id,
) -> StoreResult<()> {
    let mut active = write.open_table(DELIVERY_ACTIVE)?;
    let key = schema::delivery_active_key(host, seq, job_id.value());
    active.remove(key.as_slice())?;
    Ok(())
}

/// Insert a job and its scheduling index entry into an existing transaction.
///
/// Used by other repositories so a mutation and its follow-up job commit
/// together. For delivery kinds (`DeliverActivity`/`RelayForward`) the
/// transactional `DELIVERY_ACTIVE` gate entry is maintained in the same
/// transaction, so the FIFO gate never sees a half-committed plan.
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
    if is_delivery_kind(job.kind) {
        if let Some(host) = delivery_host_of(job) {
            insert_delivery_active(&write, &host, delivery_seq_of(job), job.id)?;
        }
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
    /// Shared enqueue path with the untrusted-input bound enforced: a
    /// payload whose serialized JSON exceeds
    /// `jobs::worker::QUARANTINE_MAX_PAYLOAD_BYTES` is never stored.
    /// Instead the row is quarantined on insert with a recorded reason and a
    /// bounded marker payload (no scheduling-index entry — quarantined rows
    /// hold none — so the job can never run).
    fn enqueue_inner(
        &self,
        kind: JobKind,
        mut payload: serde_json::Value,
        due_at_ms: u64,
        now_ms: u64,
    ) -> Result<Job> {
        let bytes: usize = serde_json::to_vec(&payload).map(|body| body.len()).unwrap_or(0);
        let write = self.begin_write()?;
        if bytes > crate::jobs::worker::QUARANTINE_MAX_PAYLOAD_BYTES {
            let mut job = Job::new(
                self.next_id(),
                kind,
                serde_json::json!({
                    "truncated": true,
                    "bytes": bytes,
                    "kept_bytes": 0,
                }),
                due_at_ms,
                now_ms,
            );
            job.state = JobState::Quarantined;
            job.last_error = Some(format!(
                "job payload exceeded {} bytes ({bytes}); stored truncated marker, job quarantined at enqueue",
                crate::jobs::worker::QUARANTINE_MAX_PAYLOAD_BYTES
            ));
            store_job(&write, &job)?;
            write.commit()?;
            return Ok(job);
        }
        // Delivery kinds mint their per-host sequence in this same transaction
        // when the caller did not stamp one (adversarial-2 C1): the mint, the
        // job row and the `DELIVERY_ACTIVE` entry commit together, so a delayed
        // insert can never let `N+1` become visible before `N`. Callers that
        // already stamped a sequence (in-transaction planning via
        // `delivery_job`, or a parked copy preserving its `seq`) keep it.
        if is_delivery_kind(kind)
            && payload.get("seq").and_then(|v| v.as_u64()).is_none()
        {
            let inbox = payload
                .get("inbox")
                .or_else(|| payload.get("relay_inbox"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if let Some(inbox) = inbox {
                let host = super::federation::delivery_host_key(&inbox);
                let seq = super::federation::next_delivery_seq_txn(&write, &host)?;
                if let Some(object) = payload.as_object_mut() {
                    object.insert("seq".into(), serde_json::Value::from(seq));
                }
            }
        }
        let job = Job::new(self.next_id(), kind, payload, due_at_ms, now_ms);
        insert_job(&write, &job)?;
        write.commit()?;
        Ok(job)
    }

    /// All job records (diagnostics and tests).
    pub fn all_jobs(&self) -> Result<Vec<Job>> {
        let read = self.begin_read()?;
        let table = read.open_table(JOBS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_k, v) = entry?;
            out.push(decode(v.value())?);
        }
        out.sort_by_key(|j: &Job| j.id);
        Ok(out)
    }

    /// Redact bearer and recipient fields from up to `limit` finished
    /// (succeeded or quarantined) mail jobs that still carry them, such as
    /// rows written before redaction existed. Returns how many were changed.
    pub fn redact_finished_job_payloads(&self, limit: usize) -> Result<usize> {
        let write = self.begin_write()?;
        let mut changed: Vec<Job> = Vec::new();
        {
            let jobs = write.open_table(JOBS)?;
            for entry in jobs.iter()? {
                let (_key, value) = entry?;
                let mut job: Job = decode(value.value())?;
                if job.state.is_terminal() && job.redact_sensitive_payload() {
                    changed.push(job);
                    if changed.len() >= limit {
                        break;
                    }
                }
            }
        }
        for job in &changed {
            store_job(&write, job)?;
        }
        write.commit()?;
        Ok(changed.len())
    }

    /// Number of entries in the scheduling index (diagnostics and tests).
    pub fn schedule_index_len(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let read = self.begin_read()?;
        let table = read.open_table(JOBS_INDEX)?;
        Ok(table.len()?)
    }
}

impl JobQueue for RedbStore {
    fn enqueue(&self, kind: JobKind, payload: serde_json::Value, now_ms: u64) -> Result<Job> {
        self.enqueue_inner(kind, payload, now_ms, now_ms)
    }

    fn enqueue_at(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        due_at_ms: u64,
        now_ms: u64,
    ) -> Result<Job> {
        self.enqueue_inner(kind, payload, due_at_ms, now_ms)
    }

    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let write = self.begin_write()?;

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
            // A leased delivery stays in the `DELIVERY_ACTIVE` gate: its
            // `(host, seq, job_id)` key does not involve the due time, so no
            // gate write is needed here.
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

    fn prune_succeeded(&self, older_than_ms: u64, limit: usize) -> Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        // Collect the victims first, then delete them in the same write
        // transaction: one `redb` txn per mutation, no network I/O inside.
        // Only `Succeeded` rows qualify; `Pending`/`Leased` rows are live
        // work and `Quarantined` rows age out through `prune_quarantined`.
        let mut victims: Vec<Job> = Vec::new();
        {
            let write = self.begin_write()?;
            {
                let jobs = write.open_table(JOBS)?;
                for entry in jobs.iter()? {
                    let (_key, value) = entry?;
                    let job: Job = decode(value.value())?;
                    if job.state == JobState::Succeeded && job.due_at_ms < older_than_ms {
                        victims.push(job);
                    }
                }
            }
            // Oldest first so a capped pass always reaps the stalest rows.
            victims.sort_by_key(|job| (job.due_at_ms, job.id));
            victims.truncate(limit);
            {
                let mut jobs = write.open_table(JOBS)?;
                for job in &victims {
                    jobs.remove(job.id.value())?;
                }
            }
            // `Succeeded` rows hold no scheduling-index entries
            // (`complete` removed them at settlement), and no gate entries
            // (`complete` removed those too). A belt-and-braces sweep drops
            // any stale gate entry a pre-index row may have left behind.
            for job in &victims {
                if is_delivery_kind(job.kind) {
                    if let Some(host) = delivery_host_of(job) {
                        remove_delivery_active(&write, &host, delivery_seq_of(job), job.id)?;
                    }
                }
            }
            write.commit()?;
        }
        Ok(victims.len() as u64)
    }

    fn prune_quarantined(&self, older_than_ms: u64, limit: usize) -> Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        // Mirror of `prune_succeeded` for the operator inspection surface:
        // only `Quarantined` rows qualify. Quarantined rows hold no
        // scheduling-index entries (`quarantine`/`retry` removed them at
        // settlement), so only the `JOBS` table entry is deleted. Prune is
        // delete-only: the row's `last_error` and redacted payload were the
        // diagnostics, and they age out with the row.
        let mut victims: Vec<Job> = Vec::new();
        {
            let write = self.begin_write()?;
            {
                let jobs = write.open_table(JOBS)?;
                for entry in jobs.iter()? {
                    let (_key, value) = entry?;
                    let job: Job = decode(value.value())?;
                    if job.state == JobState::Quarantined && job.due_at_ms < older_than_ms {
                        victims.push(job);
                    }
                }
            }
            // Oldest first so a capped pass always reaps the stalest rows.
            victims.sort_by_key(|job| (job.due_at_ms, job.id));
            victims.truncate(limit);
            {
                let mut jobs = write.open_table(JOBS)?;
                for job in &victims {
                    jobs.remove(job.id.value())?;
                }
            }
            for job in &victims {
                if is_delivery_kind(job.kind) {
                    if let Some(host) = delivery_host_of(job) {
                        remove_delivery_active(&write, &host, delivery_seq_of(job), job.id)?;
                    }
                }
            }
            write.commit()?;
        }
        Ok(victims.len() as u64)
    }

    fn complete(&self, id: Id) -> Result<()> {
        let write = self.begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        let gate = is_delivery_kind(job.kind)
            .then(|| delivery_host_of(&job).map(|host| (host, delivery_seq_of(&job))))
            .flatten();
        remove_index(&write, job.due_at_ms, id)?;
        job.state = JobState::Succeeded;
        job.lease_until_ms = None;
        // A delivered mail job keeps no bearer or recipient (WP1 1.2).
        job.redact_sensitive_payload();
        store_job(&write, &job)?;
        if let Some((host, seq)) = gate {
            remove_delivery_active(&write, &host, seq, id)?;
        }
        write.commit()?;
        Ok(())
    }

    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job> {
        let write = self.begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        let gate = is_delivery_kind(job.kind)
            .then(|| delivery_host_of(&job).map(|host| (host, delivery_seq_of(&job))))
            .flatten();
        remove_index(&write, job.due_at_ms, id)?;
        job.lease_until_ms = None;
        job.last_error = Some(error.to_string());

        if job.attempts >= job.max_attempts {
            job.state = JobState::Quarantined;
            job.redact_sensitive_payload();
            // A quarantined delivery leaves the gate: terminal rows never block.
            if let Some((host, seq)) = gate {
                remove_delivery_active(&write, &host, seq, id)?;
            }
        } else {
            job.state = JobState::Pending;
            job.due_at_ms = now_ms + backoff_ms(job.attempts);
            add_index(&write, job.due_at_ms, id)?;
            // A pending-with-backoff delivery stays in the gate: later
            // sequences keep parking until it settles or the park cap
            // relaxes the ordering (see `SEQ_PARK_MAX`).
        }
        store_job(&write, &job)?;
        write.commit()?;
        Ok(job)
    }

    fn quarantine(&self, id: Id, error: &str) -> Result<()> {
        let write = self.begin_write()?;
        let Some(mut job) = read_job_txn(&write, id)? else {
            return Err(Error::NotFound(format!("job {id}")));
        };
        let gate = is_delivery_kind(job.kind)
            .then(|| delivery_host_of(&job).map(|host| (host, delivery_seq_of(&job))))
            .flatten();
        remove_index(&write, job.due_at_ms, id)?;
        job.state = JobState::Quarantined;
        job.lease_until_ms = None;
        job.last_error = Some(error.to_string());
        job.redact_sensitive_payload();
        store_job(&write, &job)?;
        if let Some((host, seq)) = gate {
            remove_delivery_active(&write, &host, seq, id)?;
        }
        write.commit()?;
        Ok(())
    }

    fn get(&self, id: Id) -> Result<Option<Job>> {
        let read = self.begin_read()?;
        let jobs = read.open_table(JOBS)?;
        match jobs.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }
}
