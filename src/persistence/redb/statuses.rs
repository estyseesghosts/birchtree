//! Status persistence: creation, edits, tombstones, reply/context indexes,
//! pins, and status interactions (favourites, bookmarks, reblogs).
//!
//! Every mutating method opens ONE `redb` write transaction so the status row,
//! its indexes, the affected counters and any follow-up jobs commit atomically
//! (see `birchtree.md` §5). Network-facing work — notification fan-out, poll
//! expiry — is enqueued as durable jobs inside the same transaction and runs
//! after commit. Composite-key scans are bounded; `desc_id` flips an id so a
//! forward scan yields descending order.

use std::collections::HashMap;

use redb::{ReadableTable, ReadTransaction, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::application::notifications::fan_out_payload;
use crate::application::ports::{
    AccountStatusesQuery, CreatedStatus, DeletedStatus, NewStatus, StatusStore, StatusUpdate,
    StatusViewFlags,
};
use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;
use crate::domain::{
    Account, AccountStats, Error, Id, IdempotencyRecord, Job, JobKind, NotificationType, Poll,
    PollOption, QuotePolicy, QuoteRecord, QuoteState, Result, Status, StatusVersion, Visibility,
};

use super::media::{check_attachable_txn, read_media_txn, write_media_txn};
use super::schema::{
    self, ACCOUNTS, ACCOUNT_STATS, BOOKMARKS, BOOKMARKS_BY_STATUS, FAVOURITES,
    FAVOURITES_BY_STATUS, HASHTAG_INDEX, IDEMPOTENCY, MENTIONS_BY_ACCOUNT, POLL_BY_STATUS, POLLS,
    POLL_VOTES, QUOTES, QUOTES_BY_QUOTED, REBLOGS_BY_ACCOUNT, STATUSES, STATUS_AUTHOR_INDEX,
    STATUS_PARENT_INDEX, STATUS_PINS, STATUS_REBLOG_INDEX, STATUS_VERSIONS, TIMELINE_LOCAL,
    TIMELINE_PUBLIC,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Bounded scan of a status's reply-children index.
const CHILDREN_SCAN_CAP: usize = 1_000;
/// Bounded scan when tombstoning the reblogs / quotes of a status.
const CASCADE_SCAN_CAP: usize = 1_000;
/// Bounded scan when recomputing an account's `last_status_at`.
const LAST_STATUS_SCAN_CAP: usize = 200;
/// Bounded scan over one account's author index for timeline queries.
const AUTHOR_SCAN_CAP: usize = 2_000;
/// Bounded scan of an account's pins (the API caps pins at 5 anyway).
const PINS_SCAN_CAP: usize = 100;
/// Bounded scan of `favourited_by` / `reblogged_by` reverse indexes.
const INTERACTION_SCAN_CAP: usize = 500;

/// Stored shape of a pin row in [`STATUS_PINS`].
#[derive(Debug, Serialize, Deserialize)]
struct PinRecord {
    pinned_at_ms: u64,
}

// ---------------------------------------------------------------------------
// Shared transaction helpers (used by other persistence modules).
// ---------------------------------------------------------------------------

/// Read a status row inside an open write transaction.
pub(super) fn read_status_txn(
    write: &WriteTransaction,
    id: Id,
) -> StoreResult<Option<Status>> {
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
fn maintain_timeline_index(write: &WriteTransaction, status: &Status) -> StoreResult<()> {
    let desc = schema::desc_id(status.id.value());
    {
        let mut public = write.open_table(TIMELINE_PUBLIC)?;
        let mut local = write.open_table(TIMELINE_LOCAL)?;
        if status.is_deleted() || status.visibility != Visibility::Public {
            public.remove(desc)?;
            local.remove(desc)?;
            return Ok(());
        }
        public.insert(desc, ())?;
        // Author locality, read inside this transaction (the account row is
        // small and always present for local statuses).
        let accounts = write.open_table(ACCOUNTS)?;
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

// ---------------------------------------------------------------------------
// Private transaction helpers.
// ---------------------------------------------------------------------------

/// Read a status row inside an open read transaction.
fn status_at(read: &ReadTransaction, id: Id) -> StoreResult<Option<Status>> {
    let table = read.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Read a poll row inside an open write transaction.
fn read_poll_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Poll>> {
    let table = write.open_table(POLLS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Read a quote row inside an open write transaction.
fn read_quote_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<QuoteRecord>> {
    let table = write.open_table(QUOTES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Write (upsert) a quote row inside an open write transaction.
fn write_quote_txn(write: &WriteTransaction, quote: &QuoteRecord) -> StoreResult<()> {
    let mut table = write.open_table(QUOTES)?;
    table.insert(quote.id.value(), encode(quote)?.as_slice())?;
    Ok(())
}

/// Read an account's counters, defaulting when the row does not exist yet.
fn read_stats_txn(write: &WriteTransaction, account_id: Id) -> StoreResult<AccountStats> {
    let table = write.open_table(ACCOUNT_STATS)?;
    match table.get(account_id.value())? {
        Some(guard) => Ok(decode(guard.value())?),
        None => Ok(AccountStats::default()),
    }
}

/// Write (upsert) an account's counters inside an open write transaction.
fn write_stats_txn(
    write: &WriteTransaction,
    account_id: Id,
    stats: &AccountStats,
) -> StoreResult<()> {
    let mut table = write.open_table(ACCOUNT_STATS)?;
    table.insert(account_id.value(), encode(stats)?.as_slice())?;
    Ok(())
}

/// Append one edit-history snapshot inside an open write transaction.
fn insert_version(write: &WriteTransaction, version: &StatusVersion) -> StoreResult<()> {
    let mut table = write.open_table(STATUS_VERSIONS)?;
    table.insert(
        schema::pair_key(version.status_id.value(), version.version).as_slice(),
        encode(version)?.as_slice(),
    )?;
    Ok(())
}

/// Remove every ballot of a poll (prefix scan collected first, so the table is
/// not iterated while it is mutated).
fn clear_poll_votes(write: &WriteTransaction, poll_id: Id) -> StoreResult<()> {
    let keys: Vec<Vec<u8>> = {
        let votes = write.open_table(POLL_VOTES)?;
        let lo = schema::pair_key(poll_id.value(), 0);
        let hi = schema::pair_key(poll_id.value(), u64::MAX);
        let mut keys = Vec::new();
        for entry in votes.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            keys.push(key.value().to_vec());
        }
        keys
    };
    if keys.is_empty() {
        return Ok(());
    }
    let mut votes = write.open_table(POLL_VOTES)?;
    for key in &keys {
        votes.remove(key.as_slice())?;
    }
    Ok(())
}

/// Newest `created_at_ms` among the account's non-deleted statuses.
///
/// The author index yields ids in descending order, so the first live row
/// found is the answer; the scan is bounded to `LAST_STATUS_SCAN_CAP` entries.
fn newest_live_status_at(write: &WriteTransaction, account_id: Id) -> StoreResult<Option<u64>> {
    let index = write.open_table(STATUS_AUTHOR_INDEX)?;
    let lo = schema::pair_key(account_id.value(), 0);
    let hi = schema::pair_key(account_id.value(), u64::MAX);
    let mut scanned = 0usize;
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        if scanned >= LAST_STATUS_SCAN_CAP {
            break;
        }
        scanned += 1;
        let (_key, value) = entry?;
        let Some(status) = read_status_txn(write, Id::new(value.value()))? else {
            continue;
        };
        if !status.is_deleted() {
            return Ok(Some(status.created_at_ms));
        }
    }
    Ok(None)
}

/// Enqueue a `FanOutNotification` job so the notification is created after
/// commit, atomically with the interaction that produced it.
fn enqueue_fan_out(
    store: &RedbStore,
    write: &WriteTransaction,
    recipient_id: Id,
    kind: NotificationType,
    actor_id: Option<Id>,
    status_id: Option<Id>,
    now_ms: u64,
) -> StoreResult<()> {
    let job = Job::new(
        store.next_id(),
        JobKind::FanOutNotification,
        fan_out_payload(recipient_id, kind, actor_id, status_id, now_ms),
        now_ms,
        now_ms,
    );
    super::jobs::insert_job(write, &job)
}

/// Idempotency-record key: `account_id_be || sha256("status_create:{key}")[..16]`
/// (the layout documented on [`schema::IDEMPOTENCY`]), so two accounts may use
/// the same `Idempotency-Key` without colliding.
fn idempotency_key(account_id: Id, raw_key: &str) -> StoreResult<Vec<u8>> {
    let digest_hex =
        crate::application::crypto::sha256_hex(format!("status_create:{raw_key}").as_bytes());
    let digest = hex::decode(&digest_hex).expect("sha256_hex output is valid hex");
    let mut key = Vec::with_capacity(24);
    key.extend_from_slice(&account_id.value().to_be_bytes());
    key.extend_from_slice(&digest[..16]);
    Ok(key)
}

/// A reblog wrapper's `uri`/`url`. The wrapper is the booster’s own status,
/// so its links live on the booster's actor URI — never on the boosted
/// status's domain. (An activity id on another host than its actor is
/// refused by every domain-checking receiver, including ourselves.)
fn reblog_links(booster_actor_uri: &str, reblog_id: Id) -> (String, String) {
    let base = booster_actor_uri.trim_end_matches('/');
    let uri = format!("{base}/statuses/{reblog_id}");
    (uri.clone(), uri)
}

impl StatusStore for RedbStore {
    fn create_status(&self, input: NewStatus, now_ms: u64) -> Result<CreatedStatus> {
        // ONE transaction: validation, rows, indexes, counters and follow-up
        // jobs all commit (or abort) together.
        let write = self.begin_write()?;

        // -- Idempotent replay: a fresh record short-circuits with no writes.
        let replay_id = if let Some(raw) = input.idempotency_key.as_deref() {
            let key = idempotency_key(input.account_id, raw)?;
            let table = write.open_table(IDEMPOTENCY)?;
            match table.get(key.as_slice())? {
                Some(guard) => {
                    let record: IdempotencyRecord = decode(guard.value())?;
                    record.is_fresh_at(now_ms).then_some(record.result_id)
                }
                None => None,
            }
        } else {
            None
        };
        if let Some(result_id) = replay_id {
            let status = read_status_txn(&write, result_id)?
                .ok_or_else(|| Error::NotFound(format!("status {result_id}")))?;
            return Ok(CreatedStatus {
                status,
                replayed: true,
            });
        }

        // -- Media: every id must exist, be ours, and still be unattached
        //    (the new id does not exist yet, so `attach_to = None`).
        check_attachable_txn(&write, input.account_id, &input.media_ids, None)?;

        // -- Mint the identity; uri/url are `{base}/{id}`.
        let status_id = self.next_id();
        let (uri, url) = match &input.remote {
            Some(identity) => (identity.uri.clone(), identity.url.clone()),
            None => (
                format!("{}/{}", input.uri_base, status_id),
                format!("{}/{}", input.url_base, status_id),
            ),
        };

        // -- Poll, embedded with its expiry job. The polls module owns the
        //    row/index/job shapes for status creation.
        let mut poll_id: Option<Id> = None;
        if let Some(spec) = &input.poll {
            let pid = self.next_id();
            super::polls::insert_new_poll_txn(&write, pid, status_id, input.account_id, spec)?;
            super::polls::schedule_expiry_job(&write, pid, spec.expires_at_ms, now_ms)?;
            poll_id = Some(pid);
        }

        // Mentions/tags arrive resolved by the use case; the quote author is
        // added as a *silent* mention (authorization only, never serialized).
        let mentions = input.mentions.clone();
        let mut silent_mentions = input.silent_mentions.clone();
        let tags = input.tags.clone();

        // -- Quote: link rows, quoted-author counter, silent mention, job.
        //    The quotes module owns the row/index/counter shape; the caller
        //    (here) validates existence so the spec'd `NotFound` wins over the
        //    helper's defensive storage error.
        if let Some(spec) = &input.quote {
            read_status_txn(&write, spec.quoted_status_id)?
                .ok_or_else(|| Error::NotFound("quoted status".to_string()))?;
            let quote_id = self.next_id();
            super::quotes::attach_quote_txn(
                &write,
                quote_id,
                status_id,
                spec,
                input.account_id,
                now_ms,
            )?;
            silent_mentions.push(spec.quoted_account_id);
            enqueue_fan_out(
                self,
                &write,
                spec.quoted_account_id,
                NotificationType::Quote,
                Some(input.account_id),
                Some(status_id),
                now_ms,
            )?;
        }

        // -- Reply: parent index + reply counter + (non-self) mention job.
        if let Some(parent_id) = input.in_reply_to_id {
            let parent = read_status_txn(&write, parent_id)?
                .filter(|p| !p.is_deleted())
                .ok_or_else(|| Error::NotFound("in_reply_to".to_string()))?;
            {
                let mut table = write.open_table(STATUS_PARENT_INDEX)?;
                table.insert(
                    schema::pair_key(parent_id.value(), status_id.value()).as_slice(),
                    status_id.value(),
                )?;
            }
            let mut parent = parent;
            parent.replies_count = parent.replies_count.saturating_add(1);
            write_status_txn(&write, &parent)?;
            if parent.account_id != input.account_id {
                enqueue_fan_out(
                    self,
                    &write,
                    parent.account_id,
                    NotificationType::Mention,
                    Some(input.account_id),
                    Some(status_id),
                    now_ms,
                )?;
            }
        }

        // -- Mentions: reverse index per account + notification jobs.
        for mentioned in &mentions {
            {
                let mut table = write.open_table(MENTIONS_BY_ACCOUNT)?;
                table.insert(
                    schema::pair_key(mentioned.value(), status_id.value()).as_slice(),
                    (),
                )?;
            }
            if *mentioned != input.account_id {
                enqueue_fan_out(
                    self,
                    &write,
                    *mentioned,
                    NotificationType::Mention,
                    Some(input.account_id),
                    Some(status_id),
                    now_ms,
                )?;
            }
        }

        // -- Hashtags.
        for tag in &tags {
            let mut table = write.open_table(HASHTAG_INDEX)?;
            table.insert(
                schema::hashtag_key(tag, status_id.value()).as_slice(),
                status_id.value(),
            )?;
        }

        // -- Attach media rows to this status (validated above).
        for id in &input.media_ids {
            let Some(mut media) = read_media_txn(&write, *id)? else {
                return Err(Error::InvalidInput(format!(
                    "Media {id} not found or already attached to another post"
                )));
            };
            media.status_id = Some(status_id);
            write_media_txn(&write, &media)?;
        }

        // -- Author counters.
        let mut stats = read_stats_txn(&write, input.account_id)?;
        stats.statuses_count = stats.statuses_count.saturating_add(1);
        stats.last_status_at_ms = Some(stats.last_status_at_ms.map_or(now_ms, |t| t.max(now_ms)));
        write_stats_txn(&write, input.account_id, &stats)?;

        // -- The status row itself (with its author-index entry).
        let status = Status {
            id: status_id,
            account_id: input.account_id,
            uri,
            url,
            source_text: input.source_text.clone(),
            content_html: input.content_html.clone(),
            created_at_ms: now_ms,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: input.language.clone(),
            visibility: input.visibility,
            sensitive: input.sensitive,
            spoiler_text: input.spoiler_text.clone(),
            in_reply_to_id: input.in_reply_to_id,
            in_reply_to_account_id: input.in_reply_to_account_id,
            reblog_of_id: None,
            media_ids: input.media_ids.clone(),
            poll_id,
            // The use case normalizes a non-default policy through
            // `set_quote_policy` after creation; the store derives nothing.
            quote_policy: QuotePolicy::default(),
            mentions,
            silent_mentions,
            tags,
            application: input.application.clone(),
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        };
        write_status_txn(&write, &status)?;
        super::federation_content::federation_event_txn(&write, self.next_id(), input.account_id, serde_json::json!({"event": "create", "status_id": status_id.value()}), now_ms)?;
        // Phase 7.2: incremental search indexing rides the same commit.
        super::jobs::insert_job(&write, &Job::new(self.next_id(), JobKind::SearchIndexStatus, crate::jobs::search_index::index_payload(status_id), now_ms, now_ms))?;

        // -- Idempotency record, committed with the mutation it protects.
        if let Some(raw) = input.idempotency_key.as_deref() {
            let key = idempotency_key(input.account_id, raw)?;
            let record = IdempotencyRecord {
                created_at_ms: now_ms,
                scope: "status_create".to_string(),
                result_id: status_id,
            };
            let mut table = write.open_table(IDEMPOTENCY)?;
            table.insert(key.as_slice(), encode(&record)?.as_slice())?;
        }

        write.commit()?;
        Ok(CreatedStatus {
            status,
            replayed: false,
        })
    }

    fn status_by_id(&self, id: Id) -> Result<Option<Status>> {
        let read = self.begin_read()?;
        Ok(status_at(&read, id)?)
    }

    fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>> {
        let read = self.begin_read()?;
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

    fn update_status(
        &self,
        id: Id,
        account_id: Id,
        update: StatusUpdate,
        now_ms: u64,
    ) -> Result<Option<Status>> {
        let write = self.begin_write()?;

        let mut status = read_status_txn(&write, id)?
            .filter(|s| !s.is_deleted() && s.account_id == account_id)
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // -- Media: validate the target list, then diff it against the old one.
        let old_media = status.media_ids.clone();
        let media_patch = update.media_ids.clone();
        if let Some(ids) = &media_patch {
            check_attachable_txn(&write, account_id, ids, Some(id))?;
        }
        let new_media = media_patch.clone().unwrap_or_else(|| old_media.clone());
        let media_changed = media_patch.as_ref().is_some_and(|ids| *ids != old_media);

        // -- Scalar fields: `Some(patch)` participates in the change check.
        let source_changed = update
            .source_text
            .as_ref()
            .is_some_and(|v| *v != status.source_text);
        let html_changed = update
            .content_html
            .as_ref()
            .is_some_and(|v| *v != status.content_html);
        // Treat `Some("")` and `None` as the same (empty) spoiler.
        fn norm_spoiler(s: &Option<String>) -> Option<&str> {
            s.as_deref().filter(|v| !v.is_empty())
        }
        let spoiler_changed = match update.spoiler_text.as_ref() {
            Some(v) => norm_spoiler(v) != norm_spoiler(&status.spoiler_text),
            None => false,
        };
        let sensitive_changed = update.sensitive.is_some_and(|v| v != status.sensitive);
        let language_changed = update.language.as_ref().is_some_and(|v| {
            status.language.as_deref() != Some(v.as_str())
        });

        // -- Poll plan (computed against the pre-edit row; applied only if
        //    something changed).
        let old_poll: Option<Poll> = match status.poll_id {
            Some(pid) => read_poll_txn(&write, pid)?,
            None => None,
        };
        let mut poll_write: Option<Poll> = None; // row to upsert into POLLS
        let mut poll_new_id: Option<Id> = None; // freshly minted poll id
        let mut poll_clear_votes: Option<Id> = None; // reset all ballots first
        let mut poll_job_due: Option<u64> = None; // PollExpired due time
        let mut poll_changed = false;

        if let Some(spec) = &update.poll {
            let same_shape = old_poll.as_ref().is_some_and(|p| {
                p.multiple == spec.multiple
                    && p.options.len() == spec.options.len()
                    && p
                        .options
                        .iter()
                        .zip(spec.options.iter())
                        .all(|(option, title)| option.title == *title)
            });
            match old_poll.as_ref() {
                // Same options/multiple: adjust in place, no vote reset.
                Some(p) if same_shape => {
                    let mut p = p.clone();
                    let mut touched = false;
                    if p.hide_totals != spec.hide_totals {
                        p.hide_totals = spec.hide_totals;
                        touched = true;
                    }
                    if p.expires_at_ms != Some(spec.expires_at_ms) {
                        p.expires_at_ms = Some(spec.expires_at_ms);
                        touched = true;
                    }
                    if touched {
                        poll_write = Some(p);
                        poll_job_due = Some(spec.expires_at_ms);
                        poll_changed = true;
                    }
                }
                // No poll yet, or options/multiple changed: rebuild with all
                // tallies reset (Mastodon resets votes on option edits).
                existing => {
                    let mut p = match existing {
                        Some(p) => {
                            poll_clear_votes = Some(p.id);
                            p.clone()
                        }
                        None => {
                            let pid = self.next_id();
                            poll_new_id = Some(pid);
                            Poll {
                                id: pid,
                                status_id: id,
                                account_id,
                                multiple: spec.multiple,
                                hide_totals: spec.hide_totals,
                                expires_at_ms: Some(spec.expires_at_ms),
                                expired: false,
                                options: Vec::new(),
                                voters_count: 0,
                                votes_count: 0,
                            }
                        }
                    };
                    p.multiple = spec.multiple;
                    p.hide_totals = spec.hide_totals;
                    p.expires_at_ms = Some(spec.expires_at_ms);
                    p.expired = false;
                    p.options = spec
                        .options
                        .iter()
                        .map(|title| PollOption {
                            title: title.clone(),
                            votes_count: 0,
                        })
                        .collect();
                    p.voters_count = 0;
                    p.votes_count = 0;
                    poll_write = Some(p);
                    poll_job_due = Some(spec.expires_at_ms);
                    poll_changed = true;
                }
            }
        }

        if let Some(secs) = update.poll_expires_in_ms {
            // Extend the (possibly just-planned) poll without resetting votes;
            // ignored when the status has no poll.
            let current = poll_write.take().or_else(|| old_poll.clone());
            if let Some(mut p) = current {
                let new_expiry = now_ms.saturating_add(secs.saturating_mul(1000));
                if p.expires_at_ms != Some(new_expiry) {
                    p.expires_at_ms = Some(new_expiry);
                    poll_job_due = Some(new_expiry);
                    poll_changed = true;
                }
                poll_write = Some(p);
            }
        }

        // -- Nothing effective changed: abort the (still read-only) txn so no
        //    snapshot, `edited_at` bump or write happens.
        let tags_changed = update
            .tags
            .as_ref()
            .is_some_and(|tags| *tags != status.tags);
        let changed = source_changed
            || html_changed
            || spoiler_changed
            || sensitive_changed
            || language_changed
            || media_changed
            || tags_changed
            || poll_changed;
        if !changed {
            return Ok(None);
        }

        // -- Version snapshot: record the pre-edit state once as version 0.
        let mut version_count = 0u64;
        {
            let table = write.open_table(STATUS_VERSIONS)?;
            let lo = schema::pair_key(id.value(), 0);
            let hi = schema::pair_key(id.value(), u64::MAX);
            for entry in table.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, _value) = entry?;
                version_count += 1;
            }
        }
        if version_count == 0 {
            let pre_edit = StatusVersion {
                status_id: id,
                version: 0,
                account_id,
                content_html: status.content_html.clone(),
                source_text: status.source_text.clone(),
                spoiler_text: status.spoiler_text.clone(),
                sensitive: status.sensitive,
                media_ids: status.media_ids.clone(),
                poll_options: old_poll.as_ref().map(|p| {
                    p.options
                        .iter()
                        .map(|option| option.title.clone())
                        .collect()
                }),
                created_at_ms: status.created_at_ms,
            };
            insert_version(&write, &pre_edit)?;
            version_count = 1;
        }

        // -- Media: detach rows leaving the list, attach rows joining it.
        if media_changed {
            for mid in &old_media {
                if !new_media.contains(mid)
                    && let Some(mut media) = read_media_txn(&write, *mid)?
                {
                    media.status_id = None;
                    write_media_txn(&write, &media)?;
                }
            }
            for mid in &new_media {
                if !old_media.contains(mid) {
                    let Some(mut media) = read_media_txn(&write, *mid)? else {
                        return Err(Error::InvalidInput(format!(
                            "Media {mid} not found or already attached to another post"
                        )));
                    };
                    media.status_id = Some(id);
                    write_media_txn(&write, &media)?;
                }
            }
        }

        // -- Poll: mapping + ballot reset + row write + expiry job.
        if let Some(pid) = poll_new_id {
            let mut by_status = write.open_table(POLL_BY_STATUS)?;
            by_status.insert(id.value(), pid.value())?;
        }
        if let Some(pid) = poll_clear_votes {
            clear_poll_votes(&write, pid)?;
        }
        if let Some(poll) = &poll_write {
            let mut polls = write.open_table(POLLS)?;
            polls.insert(poll.id.value(), encode(poll)?.as_slice())?;
        }
        if let Some(pid) = poll_new_id {
            status.poll_id = Some(pid);
        }
        if let (Some(due), Some(poll)) = (poll_job_due, &poll_write) {
            let job = Job::new(
                self.next_id(),
                JobKind::PollExpired,
                serde_json::json!({ "poll_id": poll.id.value() }),
                due,
                now_ms,
            );
            super::jobs::insert_job(&write, &job)?;
        }

        // -- Apply the scalar fields.
        if let Some(v) = &update.source_text {
            status.source_text = v.clone();
        }
        if let Some(v) = &update.content_html {
            status.content_html = v.clone();
        }
        if update.spoiler_text.is_some() {
            status.spoiler_text = update.spoiler_text.clone().flatten().filter(|s| !s.is_empty());
        }
        if let Some(v) = update.sensitive {
            status.sensitive = v;
        }
        if let Some(v) = &update.language {
            status.language = Some(v.clone());
        }
        if media_changed {
            status.media_ids = new_media;
        }
        // -- Tags: replace the stored list and move the hashtag-index keys
        // in this same transaction (bug 4: edits used to leave both stale).
        if let Some(new_tags) = &update.tags {
            if *new_tags != status.tags {
                let mut index = write.open_table(HASHTAG_INDEX)?;
                for tag in &status.tags {
                    if !new_tags.contains(tag) {
                        index.remove(schema::hashtag_key(tag, id.value()).as_slice())?;
                    }
                }
                for tag in new_tags {
                    if !status.tags.contains(tag) {
                        index.insert(
                            schema::hashtag_key(tag, id.value()).as_slice(),
                            id.value(),
                        )?;
                    }
                }
                status.tags = new_tags.clone();
            }
        }
        // `quote_approval_policy` rides along with a real edit but is not a
        // change by itself (no snapshot, no `edited_at`); the dedicated
        // `set_quote_policy` endpoint owns standalone policy updates.
        if let Some(policy) = update.quote_approval_policy {
            status.quote_policy = match status.visibility {
                Visibility::Private | Visibility::Direct => QuotePolicy::Nobody,
                _ => policy,
            };
        }
        status.edited_at_ms = Some(now_ms);

        // -- Post-edit snapshot: version `count` (0 was just appended when the
        //    history was empty).
        let edited_poll_options: Option<Vec<String>> = match status.poll_id {
            Some(pid) => read_poll_txn(&write, pid)?.map(|p| {
                p.options
                    .iter()
                    .map(|option| option.title.clone())
                    .collect()
            }),
            None => None,
        };
        let snapshot = StatusVersion {
            status_id: id,
            version: version_count,
            account_id,
            content_html: status.content_html.clone(),
            source_text: status.source_text.clone(),
            spoiler_text: status.spoiler_text.clone(),
            sensitive: status.sensitive,
            media_ids: status.media_ids.clone(),
            poll_options: edited_poll_options,
            created_at_ms: now_ms,
        };
        insert_version(&write, &snapshot)?;

        write_status_txn(&write, &status)?;
        super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "update", "status_id": id.value()}), now_ms)?;
        // Phase 7.2: incremental search indexing rides the same commit.
        super::jobs::insert_job(&write, &Job::new(self.next_id(), JobKind::SearchIndexStatus, crate::jobs::search_index::index_payload(id), now_ms, now_ms))?;
        write.commit()?;
        Ok(Some(status))
    }

    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<DeletedStatus> {
        let write = self.begin_write()?;

        let snapshot = read_status_txn(&write, id)?
            .filter(|s| !s.is_deleted() && s.account_id == account_id)
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // Tombstone first, so the author scan below skips this status.
        let mut status = snapshot.clone();
        status.deleted_at_ms = Some(now_ms);
        write_status_txn(&write, &status)?;

        // Parent reply counter.
        if let Some(parent_id) = status.in_reply_to_id
            && let Some(mut parent) = read_status_txn(&write, parent_id)?
        {
            parent.replies_count = parent.replies_count.saturating_sub(1);
            write_status_txn(&write, &parent)?;
        }

        // Tombstone every reblog of this status and fix its author's counter.
        // Each wrapper's pre-deletion snapshot is returned for streaming.
        let mut reblog_ids: Vec<Id> = Vec::new();
        let mut cascaded: Vec<Status> = Vec::new();
        {
            let index = write.open_table(STATUS_REBLOG_INDEX)?;
            let lo = schema::pair_key(id.value(), 0);
            let hi = schema::pair_key(id.value(), u64::MAX);
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, value) = entry?;
                reblog_ids.push(Id::new(value.value()));
                if reblog_ids.len() >= CASCADE_SCAN_CAP {
                    break;
                }
            }
        }
        for reblog_id in &reblog_ids {
            let Some(mut reblog) = read_status_txn(&write, *reblog_id)? else {
                continue;
            };
            if reblog.is_deleted() {
                continue;
            }
            cascaded.push(reblog.clone());
            reblog.deleted_at_ms = Some(now_ms);
            write_status_txn(&write, &reblog)?;
            let mut stats = read_stats_txn(&write, reblog.account_id)?;
            stats.statuses_count = stats.statuses_count.saturating_sub(1);
            write_stats_txn(&write, reblog.account_id, &stats)?;
        }

        // Drop the author's pin of this status, if any.
        {
            let mut pins = write.open_table(STATUS_PINS)?;
            pins.remove(schema::pair_key(account_id.value(), id.value()).as_slice())?;
        }

        // Quotes of this status flip to `deleted`.
        let mut quote_ids: Vec<Id> = Vec::new();
        {
            let index = write.open_table(QUOTES_BY_QUOTED)?;
            let lo = schema::pair_key(id.value(), 0);
            let hi = schema::pair_key(id.value(), u64::MAX);
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, value) = entry?;
                quote_ids.push(Id::new(value.value()));
                if quote_ids.len() >= CASCADE_SCAN_CAP {
                    break;
                }
            }
        }
        for quote_id in quote_ids {
            let Some(mut quote) = read_quote_txn(&write, quote_id)? else {
                continue;
            };
            quote.state = QuoteState::Deleted;
            write_quote_txn(&write, &quote)?;
        }

        // Author counters: decrement and recompute `last_status_at` from the
        // newest surviving status (bounded scan).
        let mut stats = read_stats_txn(&write, account_id)?;
        stats.statuses_count = stats.statuses_count.saturating_sub(1);
        stats.last_status_at_ms = newest_live_status_at(&write, account_id)?;
        write_stats_txn(&write, account_id, &stats)?;

        super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "delete", "status_id": id.value()}), now_ms)?;
        // Mastodon parity (item 52): deleting a reblog wrapper must also send
        // `Undo(Announce)` so the origin decrements its reblog count. The
        // `delete` above is kept for followers' timelines.
        if let Some(target_id) = snapshot.reblog_of_id {
            write.open_table(REBLOGS_BY_ACCOUNT)?
                .remove(schema::pair_key(account_id.value(), target_id.value()).as_slice())?;
            super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "undo_announce", "status_id": target_id.value(), "announce_uri": snapshot.uri.clone()}), now_ms)?;
        }
        // Phase 7.2: incremental search indexing rides the same commit
        // (tombstoned rows and their cascaded reblogs leave the index).
        super::jobs::insert_job(&write, &Job::new(self.next_id(), JobKind::SearchIndexStatus, crate::jobs::search_index::index_payload(id), now_ms, now_ms))?;
        for reblog_id in &reblog_ids {
            super::jobs::insert_job(&write, &Job::new(self.next_id(), JobKind::SearchIndexStatus, crate::jobs::search_index::index_payload(*reblog_id), now_ms, now_ms))?;
        }
        write.commit()?;
        Ok(DeletedStatus {
            status: snapshot,
            reblogs: cascaded,
        })
    }

    fn own_reblog(&self, account_id: Id, status_id: Id) -> Result<Option<Status>> {
        let read = self.begin_read()?;
        let reblog_id = {
            let table = read.open_table(REBLOGS_BY_ACCOUNT)?;
            let key = schema::pair_key(account_id.value(), status_id.value());
            table.get(key.as_slice())?.map(|guard| Id::new(guard.value()))
        };
        let Some(reblog_id) = reblog_id else {
            return Ok(None);
        };
        Ok(status_at(&read, reblog_id)?.filter(|s| !s.is_deleted()))
    }

    fn status_children(
        &self,
        parent_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Status>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;

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
            self.note_scanned(ids.len());
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

    fn status_versions(&self, status_id: Id) -> Result<Vec<StatusVersion>> {
        let read = self.begin_read()?;
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

    fn statuses_by_account(
        &self,
        account_id: Id,
        query: AccountStatusesQuery,
    ) -> Result<Vec<Status>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;

        if query.pinned_only {
            // Pin records first (pin order, newest pin first), then rows.
            let mut pinned: Vec<(u64, Id)> = Vec::new();
            {
                let pins = read.open_table(STATUS_PINS)?;
                let lo = schema::pair_key(account_id.value(), 0);
                let hi = schema::pair_key(account_id.value(), u64::MAX);
                for entry in pins.range(lo.as_slice()..=hi.as_slice())? {
                    let (key, value) = entry?;
                    let bytes = key.value();
                    let status_id =
                        Id::new(u64::from_be_bytes(bytes[8..16].try_into().unwrap()));
                    let record: PinRecord = decode(value.value())?;
                    pinned.push((record.pinned_at_ms, status_id));
                    if pinned.len() >= PINS_SCAN_CAP {
                        break;
                    }
                }
            }
            pinned.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

            let mut out = Vec::new();
            let statuses = read.open_table(STATUSES)?;
            for (_, status_id) in pinned {
                if let Some(m) = query.max_id
                    && status_id >= m
                {
                    continue;
                }
                let Some(guard) = statuses.get(status_id.value())? else {
                    continue;
                };
                let status: Status = decode(guard.value())?;
                if status.is_deleted() {
                    continue;
                }
                out.push(status);
                if out.len() >= query.limit {
                    break;
                }
            }
            return Ok(out);
        }

        // Author index: keys are `(account || desc(id))`, so a forward scan
        // yields status ids descending. With `min_id` (and no `since_id`) the
        // oldest matching ids are wanted, so keep scanning and retain the
        // last `limit` matches instead of stopping at the first `limit`.
        let want_oldest = query.min_id.is_some() && query.since_id.is_none();
        let mut matched: Vec<Status> = Vec::new();
        {
            let index = read.open_table(STATUS_AUTHOR_INDEX)?;
            let statuses = read.open_table(STATUSES)?;
            let lo = schema::pair_key(account_id.value(), 0);
            let hi = schema::pair_key(account_id.value(), u64::MAX);
            let mut scanned = 0usize;
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                if scanned >= AUTHOR_SCAN_CAP {
                    break;
                }
                scanned += 1;
                let (key, _value) = entry?;
                let bytes = key.value();
                let desc = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
                let id = Id::new(u64::MAX - desc);
                if query.max_id.is_some_and(|m| id >= m)
                    || query.since_id.is_some_and(|s| id <= s)
                    || query.min_id.is_some_and(|m| id <= m)
                {
                    continue;
                }
                let Some(guard) = statuses.get(id.value())? else {
                    continue;
                };
                let status: Status = decode(guard.value())?;
                if status.is_deleted() {
                    continue;
                }
                if query.exclude_replies && status.in_reply_to_id.is_some() {
                    continue;
                }
                if query.exclude_reblogs && status.is_reblog() {
                    continue;
                }
                matched.push(status);
                if !want_oldest && matched.len() >= query.limit {
                    break;
                }
            }
            self.note_scanned(scanned);
        }
        if want_oldest && matched.len() > query.limit {
            matched.drain(..matched.len() - query.limit);
        }
        Ok(matched)
    }

    fn viewer_status_flags(
        &self,
        viewer_id: Id,
        status_ids: &[Id],
    ) -> Result<HashMap<Id, StatusViewFlags>> {
        let read = self.begin_read()?;
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

    fn favourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let mut status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // Idempotent: the pair key exists → already favourited, no counter change.
        let already = {
            let table = write.open_table(FAVOURITES)?;
            table
                .get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
                .is_some()
        };
        if already {
            return Ok(status);
        }

        let favourite = Favourite {
            id: self.next_id(),
            account_id,
            status_id,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(FAVOURITES)?;
            table.insert(
                schema::pair_key(account_id.value(), status_id.value()).as_slice(),
                encode(&favourite)?.as_slice(),
            )?;
        }
        {
            let mut table = write.open_table(FAVOURITES_BY_STATUS)?;
            table.insert(
                schema::pair_key(status_id.value(), favourite.id.value()).as_slice(),
                account_id.value(),
            )?;
        }

        status.favourites_count = status.favourites_count.saturating_add(1);
        write_status_txn(&write, &status)?;
        if status.account_id != account_id {
            enqueue_fan_out(
                self,
                &write,
                status.account_id,
                NotificationType::Favourite,
                Some(account_id),
                Some(status_id),
                now_ms,
            )?;
        }

        super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "like", "status_id": status_id.value()}), now_ms)?;
        write.commit()?;
        Ok(status)
    }

    fn unfavourite_status(&self, account_id: Id, status_id: Id, _now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let mut status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        let favourite: Option<Favourite> = {
            let table = write.open_table(FAVOURITES)?;
            match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        if let Some(favourite) = favourite {
            {
                let mut table = write.open_table(FAVOURITES)?;
                table.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
            }
            {
                let mut table = write.open_table(FAVOURITES_BY_STATUS)?;
                table.remove(
                    schema::pair_key(status_id.value(), favourite.id.value()).as_slice(),
                )?;
            }
            status.favourites_count = status.favourites_count.saturating_sub(1);
            write_status_txn(&write, &status)?;
            super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "unlike", "status_id": status_id.value()}), _now_ms)?;
            // Notification deletion belongs to the notification read surface
            // (Phase 4); the interaction never rewrites notification rows here.
        }

        write.commit()?;
        Ok(status)
    }

    fn bookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        let already = {
            let table = write.open_table(BOOKMARKS)?;
            table
                .get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
                .is_some()
        };
        if already {
            return Ok(status);
        }

        let bookmark = Bookmark {
            id: self.next_id(),
            account_id,
            status_id,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(BOOKMARKS)?;
            table.insert(
                schema::pair_key(account_id.value(), status_id.value()).as_slice(),
                encode(&bookmark)?.as_slice(),
            )?;
        }
        {
            let mut table = write.open_table(BOOKMARKS_BY_STATUS)?;
            table.insert(
                schema::pair_key(status_id.value(), bookmark.id.value()).as_slice(),
                account_id.value(),
            )?;
        }

        write.commit()?;
        Ok(status)
    }

    fn unbookmark_status(&self, account_id: Id, status_id: Id, _now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        let bookmark: Option<Bookmark> = {
            let table = write.open_table(BOOKMARKS)?;
            match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        if let Some(bookmark) = bookmark {
            {
                let mut table = write.open_table(BOOKMARKS)?;
                table.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
            }
            {
                let mut table = write.open_table(BOOKMARKS_BY_STATUS)?;
                table.remove(
                    schema::pair_key(status_id.value(), bookmark.id.value()).as_slice(),
                )?;
            }
        }

        write.commit()?;
        Ok(status)
    }

    fn reblog_status(
        &self,
        account_id: Id,
        status_id: Id,
        visibility: Visibility,
        now_ms: u64,
    ) -> Result<Status> {
        let write = self.begin_write()?;

        let mut target = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // Idempotent: return the viewer's existing live reblog. A tombstoned
        // reblog (after an unreblog) does not satisfy the request — fall
        // through and mint a fresh one, overwriting the account->target map.
        let existing_id: Option<Id> = {
            let table = write.open_table(REBLOGS_BY_ACCOUNT)?;
            match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
                Some(guard) => Some(Id::new(guard.value())),
                None => None,
            }
        };
        if let Some(reblog_id) = existing_id
            && let Some(row) = read_status_txn(&write, reblog_id)?
            && !row.is_deleted()
        {
            return Ok(row);
        }

        let reblog_id = self.next_id();
        // The wrapper's links live on the booster's actor URI (see
        // `reblog_links`); the target's links stay untouched.
        let booster_actor_uri = {
            let accounts = write.open_table(ACCOUNTS)?;
            match accounts.get(account_id.value())? {
                Some(guard) => {
                    let account: Account = decode(guard.value())?;
                    account.actor_uri
                }
                None => return Err(Error::NotFound("reblogging account".into())),
            }
        };
        let (uri, url) = reblog_links(&booster_actor_uri, reblog_id);
        let reblog = Status {
            id: reblog_id,
            account_id,
            uri,
            url,
            source_text: String::new(),
            content_html: String::new(),
            created_at_ms: now_ms,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: None,
            visibility,
            sensitive: false,
            spoiler_text: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: Some(target.id),
            media_ids: Vec::new(),
            poll_id: None,
            quote_policy: QuotePolicy::Nobody,
            mentions: Vec::new(),
            silent_mentions: Vec::new(),
            tags: Vec::new(),
            application: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        };
        write_status_txn(&write, &reblog)?;
        {
            let mut table = write.open_table(STATUS_REBLOG_INDEX)?;
            table.insert(
                schema::pair_key(target.id.value(), reblog_id.value()).as_slice(),
                reblog_id.value(),
            )?;
        }
        {
            let mut table = write.open_table(REBLOGS_BY_ACCOUNT)?;
            table.insert(
                schema::pair_key(account_id.value(), target.id.value()).as_slice(),
                reblog_id.value(),
            )?;
        }

        target.reblogs_count = target.reblogs_count.saturating_add(1);
        write_status_txn(&write, &target)?;

        let mut stats = read_stats_txn(&write, account_id)?;
        stats.statuses_count = stats.statuses_count.saturating_add(1);
        stats.last_status_at_ms = Some(stats.last_status_at_ms.map_or(now_ms, |t| t.max(now_ms)));
        write_stats_txn(&write, account_id, &stats)?;

        if target.account_id != account_id {
            enqueue_fan_out(
                self,
                &write,
                target.account_id,
                NotificationType::Reblog,
                Some(account_id),
                Some(target.id),
                now_ms,
            )?;
        }

        super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "announce", "status_id": target.id.value()}), now_ms)?;
        write.commit()?;
        Ok(reblog)
    }

    fn unreblog_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let mut target = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // Never reblogged → no-op returning the original.
        let reblog_id: Option<Id> = {
            let table = write.open_table(REBLOGS_BY_ACCOUNT)?;
            match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
                Some(guard) => Some(Id::new(guard.value())),
                None => None,
            }
        };
        let Some(reblog_id) = reblog_id else {
            return Ok(target);
        };
        let Some(mut reblog) = read_status_txn(&write, reblog_id)? else {
            return Ok(target);
        };
        if reblog.is_deleted() {
            // Self-heal a stale mapping (tombstoned wrapper, row still present).
            write.open_table(REBLOGS_BY_ACCOUNT)?
                .remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
            write.commit()?;
            return Ok(target);
        }

        reblog.deleted_at_ms = Some(now_ms);
        write_status_txn(&write, &reblog)?;
        target.reblogs_count = target.reblogs_count.saturating_sub(1);
        write_status_txn(&write, &target)?;

        // Drop the account->reblog mapping with the wrapper: `reblogged` flips
        // back to false, and a later re-reblog mints a fresh wrapper (the old
        // row stays tombstoned for referential integrity).
        write.open_table(REBLOGS_BY_ACCOUNT)?
            .remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;

        let mut stats = read_stats_txn(&write, reblog.account_id)?;
        stats.statuses_count = stats.statuses_count.saturating_sub(1);
        write_stats_txn(&write, reblog.account_id, &stats)?;

        super::federation_content::federation_event_txn(&write, self.next_id(), account_id, serde_json::json!({"event": "undo_announce", "status_id": status_id.value(), "announce_uri": reblog.uri.clone()}), now_ms)?;
        write.commit()?;
        Ok(target)
    }

    fn pin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;
        if status.account_id != account_id {
            // `StatusPinValidator` adds the `ownership` error; Mastodon wraps
            // it via `RecordInvalid` -> 422.
            return Err(Error::InvalidInput(
                "Validation failed: Someone else's post cannot be pinned".to_string(),
            ));
        }
        if status.is_reblog() {
            return Err(Error::InvalidInput(
                "Validation failed: A boost cannot be pinned".to_string(),
            ));
        }
        if status.visibility == Visibility::Direct {
            return Err(Error::InvalidInput(
                "Validation failed: Posts that are only visible to mentioned users cannot be pinned"
                    .to_string(),
            ));
        }

        // Idempotent: already pinned.
        let already = {
            let pins = write.open_table(STATUS_PINS)?;
            pins.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
                .is_some()
        };
        if already {
            return Ok(status);
        }

        // Pin limit (Mastodon allows 5).
        let mut count = 0usize;
        {
            let pins = write.open_table(STATUS_PINS)?;
            let lo = schema::pair_key(account_id.value(), 0);
            let hi = schema::pair_key(account_id.value(), u64::MAX);
            for entry in pins.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, _value) = entry?;
                count += 1;
                if count >= PINS_SCAN_CAP {
                    break;
                }
            }
        }
        if count >= 5 {
            return Err(Error::InvalidInput(
                "Validation failed: You have already pinned the maximum number of posts"
                    .to_string(),
            ));
        }

        let record = PinRecord {
            pinned_at_ms: now_ms,
        };
        {
            let mut pins = write.open_table(STATUS_PINS)?;
            pins.insert(
                schema::pair_key(account_id.value(), status_id.value()).as_slice(),
                encode(&record)?.as_slice(),
            )?;
        }

        write.commit()?;
        Ok(status)
    }

    fn unpin_status(&self, account_id: Id, status_id: Id, _now_ms: u64) -> Result<Status> {
        let write = self.begin_write()?;

        let status = read_status_txn(&write, status_id)?
            .ok_or_else(|| Error::NotFound("status".to_string()))?;
        if status.account_id != account_id {
            return Err(Error::NotFound("status".to_string()));
        }

        {
            let mut pins = write.open_table(STATUS_PINS)?;
            pins.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
        }

        write.commit()?;
        Ok(status)
    }

    fn set_quote_policy(&self, status_id: Id, policy: QuotePolicy) -> Result<Status> {
        let write = self.begin_write()?;

        let mut status = read_status_txn(&write, status_id)?
            .filter(|s| !s.is_deleted())
            .ok_or_else(|| Error::NotFound("status".to_string()))?;

        // Private/direct statuses can never be quotable, regardless of the
        // requested policy (mirrors the downgrade in the use case/serializer).
        status.quote_policy = match status.visibility {
            Visibility::Private | Visibility::Direct => QuotePolicy::Nobody,
            _ => policy,
        };
        write_status_txn(&write, &status)?;

        write.commit()?;
        Ok(status)
    }

    fn favourited_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(FAVOURITES_BY_STATUS)?;

        // `(status || favourite_id)` ascending → collect (bounded), reverse
        // for newest-first, then apply the cursor.
        let mut rows: Vec<(Id, Id)> = Vec::new();
        {
            let lo = schema::pair_key(status_id.value(), 0);
            let hi = schema::pair_key(status_id.value(), u64::MAX);
            for entry in table.range(lo.as_slice()..=hi.as_slice())? {
                let (key, value) = entry?;
                let bytes = key.value();
                let favourite_id = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
                rows.push((Id::new(value.value()), Id::new(favourite_id)));
                if rows.len() >= INTERACTION_SCAN_CAP {
                    break;
                }
            }
        }
        rows.reverse();
        Ok(rows
            .into_iter()
            .filter(|(_, favourite_id)| max_id.is_none_or(|m| *favourite_id < m))
            .take(limit)
            .collect())
    }

    fn reblogged_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;

        // `(status || reblog_id)` ascending → collect (bounded), reverse.
        let reblog_ids: Vec<Id> = {
            let index = read.open_table(STATUS_REBLOG_INDEX)?;
            let lo = schema::pair_key(status_id.value(), 0);
            let hi = schema::pair_key(status_id.value(), u64::MAX);
            let mut ids = Vec::new();
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, value) = entry?;
                ids.push(Id::new(value.value()));
                if ids.len() >= INTERACTION_SCAN_CAP {
                    break;
                }
            }
            ids
        };

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
            let reblog: Status = decode(guard.value())?;
            if reblog.is_deleted() {
                continue;
            }
            out.push((reblog.account_id, reblog_id));
        }
        Ok(out)
    }

    fn favourited_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Favourite>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(FAVOURITES)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut rows: Vec<Favourite> = Vec::new();
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            rows.push(decode(value.value())?);
            if rows.len() >= INTERACTION_SCAN_CAP {
                break;
            }
        }
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(rows
            .into_iter()
            .filter(|fav| max_id.is_none_or(|m| fav.id < m))
            .filter(|fav| since_id.is_none_or(|m| fav.id > m))
            .filter(|fav| min_id.is_none_or(|m| fav.id > m))
            .take(limit)
            .collect())
    }

    fn bookmarked_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Bookmark>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(BOOKMARKS)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut rows: Vec<Bookmark> = Vec::new();
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            rows.push(decode(value.value())?);
            if rows.len() >= INTERACTION_SCAN_CAP {
                break;
            }
        }
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(rows
            .into_iter()
            .filter(|mark| max_id.is_none_or(|m| mark.id < m))
            .filter(|mark| since_id.is_none_or(|m| mark.id > m))
            .filter(|mark| min_id.is_none_or(|m| mark.id > m))
            .take(limit)
            .collect())
    }

    fn backfill_tag_batch(
        &self,
        after: Option<Id>,
        cap: usize,
    ) -> Result<Vec<(Id, Vec<String>, String)>> {
        let cap = cap.clamp(1, 2_000);
        let read = self.begin_read()?;
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

    fn rewrite_status_tags(&self, status_id: Id, new_tags: &[String]) -> Result<()> {
        let write = self.begin_write()?;
        let old_tags: Vec<String> = {
            let table = write.open_table(STATUSES)?;
            match table.get(status_id.value())? {
                Some(guard) => {
                    let status: Status = decode(guard.value())?;
                    status.tags.clone()
                }
                None => return Ok(()),
            }
        };
        {
            let mut index = write.open_table(HASHTAG_INDEX)?;
            for tag in &old_tags {
                if !new_tags.contains(tag) {
                    index.remove(schema::hashtag_key(tag, status_id.value()).as_slice())?;
                }
            }
            for tag in new_tags {
                if !old_tags.contains(tag) {
                    index.insert(
                        schema::hashtag_key(tag, status_id.value()).as_slice(),
                        status_id.value(),
                    )?;
                }
            }
        }
        {
            let mut table = write.open_table(STATUSES)?;
            let updated: Option<Status> = table
                .get(status_id.value())?
                .map(|guard| {
                    let mut status: Status = decode(guard.value())?;
                    status.tags = new_tags.to_vec();
                    Ok::<_, crate::domain::Error>(status)
                })
                .transpose()?;
            if let Some(status) = updated {
                table.insert(status_id.value(), encode(&status)?.as_slice())?;
            }
        }
        write.commit()?;
        Ok(())
    }
}
