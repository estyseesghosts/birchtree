//! Status creation: validation, rows, indexes, counters and follow-up jobs.
//!
//! `create_status_impl` opens ONE `redb` write transaction; the stages below
//! each take that transaction so rows, indexes, counters and jobs commit (or
//! abort) together:
//!
//! * [`insert_row_txn`] — validation, identity, poll and quote/reply rows.
//! * [`write_indexes_txn`] — mention/hashtag/media indexes, counters, the
//!   status row itself.
//! * [`enqueue_create_jobs_txn`] — notification fan-out, follower fan-out,
//!   federation and search-index jobs, and the idempotency record.
//!
//! Helpers owned here (`idempotency_key`, `read_stats_txn`,
//! `write_stats_txn`, `enqueue_fan_out`) are imported by the sibling
//! `statuses_*` modules; nothing is duplicated.

use redb::{ReadableTable, WriteTransaction};

use crate::application::notifications::fan_out_payload;
use crate::application::ports::{CreatedStatus, NewStatus};
use crate::domain::{
    AccountStats, Error, Id, IdempotencyRecord, Job, JobKind, NotificationType, QuotePolicy,
    Result, Status,
};

use super::media::{check_attachable_txn, read_media_txn, write_media_txn};
use super::schema::{
    self, ACCOUNT_STATS, HASHTAG_INDEX, IDEMPOTENCY, MENTIONS_BY_ACCOUNT, STATUS_PARENT_INDEX,
    STATUSES,
};
use super::statuses_read::{ensure_id_unused, read_status_txn, write_status_txn};
use super::{RedbStore, StoreResult, decode, encode};

/// Identity, poll and relation state minted before the status row is written.
pub(super) struct PreparedCreate {
    pub(super) status_id: Id,
    pub(super) uri: String,
    pub(super) url: String,
    pub(super) poll_id: Option<Id>,
    pub(super) mentions: Vec<Id>,
    pub(super) silent_mentions: Vec<Id>,
    pub(super) tags: Vec<String>,
    /// Author to notify with a `Quote` notification, if a quote was attached.
    pub(super) quoted_account: Option<Id>,
    /// Parent author to notify with a `Mention`, if a non-self reply.
    pub(super) reply_notify: Option<Id>,
}

/// Idempotency-record key: `account_id_be || sha256("status_create:{key}")[..16]`
/// (the layout documented on [`schema::IDEMPOTENCY`]), so two accounts may use
/// the same `Idempotency-Key` without colliding.
pub(super) fn idempotency_key(account_id: Id, raw_key: &str) -> StoreResult<Vec<u8>> {
    let digest_hex =
        crate::application::crypto::sha256_hex(format!("status_create:{raw_key}").as_bytes());
    let digest = hex::decode(&digest_hex).expect("sha256_hex output is valid hex");
    let mut key = Vec::with_capacity(24);
    key.extend_from_slice(&account_id.value().to_be_bytes());
    key.extend_from_slice(&digest[..16]);
    Ok(key)
}

/// Read an account's counters, defaulting when the row does not exist yet.
pub(super) fn read_stats_txn(
    write: &WriteTransaction,
    account_id: Id,
) -> StoreResult<AccountStats> {
    let table = write.open_table(ACCOUNT_STATS)?;
    match table.get(account_id.value())? {
        Some(guard) => Ok(decode(guard.value())?),
        None => Ok(AccountStats::default()),
    }
}

/// Write (upsert) an account's counters inside an open write transaction.
pub(super) fn write_stats_txn(
    write: &WriteTransaction,
    account_id: Id,
    stats: &AccountStats,
) -> StoreResult<()> {
    let mut table = write.open_table(ACCOUNT_STATS)?;
    table.insert(account_id.value(), encode(stats)?.as_slice())?;
    Ok(())
}

/// Enqueue a `FanOutNotification` job so the notification is created after
/// commit, atomically with the interaction that produced it.
pub(super) fn enqueue_fan_out(
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

/// Validate the input, mint the identity, and write the poll, quote and reply
/// rows (parent index + parent counter) inside the creation transaction.
pub(super) fn insert_row_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    input: &NewStatus,
    now_ms: u64,
) -> Result<PreparedCreate> {
    // -- Media: every id must exist, be ours, and still be unattached
    //    (the new id does not exist yet, so `attach_to = None`).
    check_attachable_txn(write, input.account_id, &input.media_ids, None)?;

    // -- Mint the identity; uri/url are `{base}/{id}`.
    let status_id = store.next_id();
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
        let pid = store.next_id();
        super::polls::insert_new_poll_txn(write, pid, status_id, input.account_id, spec)?;
        super::polls::schedule_expiry_job(write, pid, spec.expires_at_ms, now_ms)?;
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
    let mut quoted_account: Option<Id> = None;
    if let Some(spec) = &input.quote {
        read_status_txn(write, spec.quoted_status_id)?
            .ok_or_else(|| Error::NotFound("quoted status".to_string()))?;
        let quote_id = store.next_id();
        super::quotes::attach_quote_txn(
            write,
            quote_id,
            status_id,
            spec,
            input.account_id,
            now_ms,
        )?;
        silent_mentions.push(spec.quoted_account_id);
        quoted_account = Some(spec.quoted_account_id);
    }

    // -- Reply: parent index + reply counter (+ a (non-self) mention job,
    //    enqueued later by `enqueue_create_jobs_txn`).
    let mut reply_notify: Option<Id> = None;
    if let Some(parent_id) = input.in_reply_to_id {
        let parent = read_status_txn(write, parent_id)?
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
        write_status_txn(write, &parent)?;
        if parent.account_id != input.account_id {
            reply_notify = Some(parent.account_id);
        }
    }

    Ok(PreparedCreate {
        status_id,
        uri,
        url,
        poll_id,
        mentions,
        silent_mentions,
        tags,
        quoted_account,
        reply_notify,
    })
}

/// Write the mention/hashtag/media indexes, bump the author counters and
/// insert the status row itself.
pub(super) fn write_indexes_txn(
    write: &WriteTransaction,
    input: &NewStatus,
    prepared: &PreparedCreate,
    now_ms: u64,
) -> Result<Status> {
    let status_id = prepared.status_id;

    // -- Mentions: reverse index per account (notification jobs ride the
    //    later `enqueue_create_jobs_txn` stage).
    for mentioned in &prepared.mentions {
        let mut table = write.open_table(MENTIONS_BY_ACCOUNT)?;
        table.insert(
            schema::pair_key(mentioned.value(), status_id.value()).as_slice(),
            (),
        )?;
    }

    // -- Hashtags.
    for tag in &prepared.tags {
        let mut table = write.open_table(HASHTAG_INDEX)?;
        table.insert(
            schema::hashtag_key(tag, status_id.value()).as_slice(),
            status_id.value(),
        )?;
    }

    // -- Attach media rows to this status (validated above).
    for id in &input.media_ids {
        let Some(mut media) = read_media_txn(write, *id)? else {
            return Err(Error::InvalidInput(format!(
                "Media {id} not found or already attached to another post"
            )));
        };
        media.status_id = Some(status_id);
        write_media_txn(write, &media)?;
    }

    // -- Author counters.
    let mut stats = read_stats_txn(write, input.account_id)?;
    stats.statuses_count = stats.statuses_count.saturating_add(1);
    stats.last_status_at_ms = Some(stats.last_status_at_ms.map_or(now_ms, |t| t.max(now_ms)));
    write_stats_txn(write, input.account_id, &stats)?;

    // -- The status row itself (with its author-index entry).
    let status = Status {
        id: status_id,
        account_id: input.account_id,
        uri: prepared.uri.clone(),
        url: prepared.url.clone(),
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
        place_id: input.place_id,
        poll_id: prepared.poll_id,
        // The use case normalizes a non-default policy through
        // `set_quote_policy` after creation; the store derives nothing.
        quote_policy: QuotePolicy::default(),
        mentions: prepared.mentions.clone(),
        silent_mentions: prepared.silent_mentions.clone(),
        tags: prepared.tags.clone(),
        application: input.application.clone(),
        replies_count: 0,
        reblogs_count: 0,
        favourites_count: 0,
        quotes_count: 0,
    };
    ensure_id_unused(write, STATUSES, status_id)?;
    write_status_txn(write, &status)?;
    Ok(status)
}

/// Enqueue the creation's follow-up jobs in their original relative order
/// (quote fan-out, reply fan-out, mention fan-outs, follower fan-out,
/// federation event, search-index job) plus the idempotency record.
pub(super) fn enqueue_create_jobs_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    input: &NewStatus,
    prepared: &PreparedCreate,
    now_ms: u64,
) -> Result<()> {
    let status_id = prepared.status_id;

    super::statuses_create_stages::fan_out_quote_reply_txn(store, write, input, prepared, now_ms)?;
    super::statuses_create_stages::fan_out_followers_txn(store, write, input, status_id, now_ms)?;
    super::statuses_create_stages::federation_search_txn(
        store,
        write,
        input.account_id,
        status_id,
        now_ms,
    )?;
    super::statuses_create_stages::write_idempotency_txn(write, input, status_id, now_ms)?;
    Ok(())
}

/// Create a status with its poll/quote, indexes, counters, idempotency
/// record and follow-up jobs in ONE transaction.
pub(super) fn create_status_impl(
    store: &RedbStore,
    input: NewStatus,
    now_ms: u64,
) -> Result<CreatedStatus> {
    // ONE transaction: validation, rows, indexes, counters and follow-up
    // jobs all commit (or abort) together.
    let write = store.begin_write()?;

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

    let prepared = insert_row_txn(store, &write, &input, now_ms)?;
    let status = write_indexes_txn(&write, &input, &prepared, now_ms)?;
    enqueue_create_jobs_txn(store, &write, &input, &prepared, now_ms)?;

    write.commit()?;
    Ok(CreatedStatus {
        status,
        replayed: false,
    })
}
