//! Status edits: field diffing, poll planning, patch application and jobs.
//!
//! `update_status_impl` opens ONE `redb` write transaction; the stages below
//! each take that transaction:
//!
//! * [`diff_media`] / [`diff_scalars`] — pure(ish) change detection.
//! * [`plan_poll`] — compute the poll row/ballot/job plan (no writes).
//! * [`apply_patch_txn`] — snapshots, media/poll/scalar/tag writes.
//! * [`enqueue_edit_jobs_txn`] — federation, reblogger/quoter and search jobs.
//!
//! The tag-backfill writer ([`rewrite_status_tags_impl`]) also lives here:
//! it moves hashtag-index keys exactly like the tag block of the edit patch.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::StatusUpdate;
use crate::domain::{Error, Id, Job, JobKind, Poll, Result, Status, StatusVersion};

use super::media::check_attachable_txn;
use super::schema::{self, HASHTAG_INDEX, POLLS, QUOTES_BY_QUOTED, STATUS_VERSIONS};
use super::statuses_read::{read_status_txn, write_status_txn};
use super::{RedbStore, StoreResult, decode, encode};

/// Which scalar fields of an edit patch differ from the stored row.
pub(super) struct ScalarDiff {
    source_changed: bool,
    html_changed: bool,
    spoiler_changed: bool,
    sensitive_changed: bool,
    language_changed: bool,
    tags_changed: bool,
    place_changed: bool,
}

/// The poll side of an edit, computed before the change check.
pub(super) struct PollPlan {
    /// Row to upsert into `POLLS`.
    pub(super) poll_write: Option<Poll>,
    /// Freshly minted poll id (needs a `POLL_BY_STATUS` mapping).
    pub(super) poll_new_id: Option<Id>,
    /// Poll whose ballots must be reset first.
    pub(super) poll_clear_votes: Option<Id>,
    /// `PollExpired` due time.
    pub(super) poll_job_due: Option<u64>,
    pub(super) poll_changed: bool,
}

/// Treat `Some("")` and `None` as the same (empty) spoiler.
fn norm_spoiler_text(spoiler: &Option<String>) -> Option<&str> {
    spoiler.as_deref().filter(|v| !v.is_empty())
}

/// Read a poll row inside an open write transaction.
pub(super) fn read_poll_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Poll>> {
    let table = write.open_table(POLLS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Append one edit-history snapshot inside an open write transaction.
pub(super) fn insert_version(write: &WriteTransaction, version: &StatusVersion) -> StoreResult<()> {
    let mut table = write.open_table(STATUS_VERSIONS)?;
    table.insert(
        schema::pair_key(version.status_id.value(), version.version).as_slice(),
        encode(version)?.as_slice(),
    )?;
    Ok(())
}

/// Validate the target media list and diff it against the stored one.
///
/// Returns the list to store and whether it differs.
pub(super) fn diff_media(
    write: &WriteTransaction,
    account_id: Id,
    status_id: Id,
    media_patch: Option<&[Id]>,
    old_media: &[Id],
) -> Result<(Vec<Id>, bool)> {
    if let Some(ids) = media_patch {
        check_attachable_txn(write, account_id, ids, Some(status_id))?;
    }
    let new_media = media_patch
        .map(<[Id]>::to_vec)
        .unwrap_or_else(|| old_media.to_vec());
    let media_changed = media_patch.is_some_and(|ids| ids != old_media);
    Ok((new_media, media_changed))
}

/// Compare the scalar patch fields against the stored row.
pub(super) fn diff_scalars(status: &Status, update: &StatusUpdate) -> ScalarDiff {
    let source_changed = update
        .source_text
        .as_ref()
        .is_some_and(|v| *v != status.source_text);
    let html_changed = update
        .content_html
        .as_ref()
        .is_some_and(|v| *v != status.content_html);
    let spoiler_changed = match update.spoiler_text.as_ref() {
        Some(v) => norm_spoiler_text(v) != norm_spoiler_text(&status.spoiler_text),
        None => false,
    };
    let sensitive_changed = update.sensitive.is_some_and(|v| v != status.sensitive);
    let language_changed = update
        .language
        .as_ref()
        .is_some_and(|v| status.language.as_deref() != Some(v.as_str()));
    let tags_changed = update
        .tags
        .as_ref()
        .is_some_and(|tags| *tags != status.tags);
    let place_changed = update
        .place_id
        .is_some_and(|place| place != status.place_id);
    ScalarDiff {
        source_changed,
        html_changed,
        spoiler_changed,
        sensitive_changed,
        language_changed,
        tags_changed,
        place_changed,
    }
}

/// Compute the poll plan against the pre-edit row (applied only if something
/// changed). Returns the stored poll (for the pre-edit snapshot) and the plan.
pub(super) fn plan_poll(
    store: &RedbStore,
    write: &WriteTransaction,
    status: &Status,
    update: &StatusUpdate,
    now_ms: u64,
) -> Result<(Option<Poll>, PollPlan)> {
    // -- Poll plan (computed against the pre-edit row; applied only if
    //    something changed).
    let old_poll: Option<Poll> = match status.poll_id {
        Some(pid) => read_poll_txn(write, pid)?,
        None => None,
    };
    let mut plan = PollPlan {
        poll_write: None,
        poll_new_id: None,
        poll_clear_votes: None,
        poll_job_due: None,
        poll_changed: false,
    };

    if let Some(spec) = &update.poll {
        match old_poll.as_ref() {
            // Same options/multiple: adjust in place, no vote reset.
            Some(p) if super::statuses_update_stages::same_shape_poll(p, spec) => {
                if let Some((row, due)) =
                    super::statuses_update_stages::adjust_poll_in_place(p, spec)
                {
                    plan.poll_write = Some(row);
                    plan.poll_job_due = Some(due);
                    plan.poll_changed = true;
                }
            }
            // No poll yet, or options/multiple changed: rebuild with all
            // tallies reset (Mastodon resets votes on option edits).
            existing => {
                let (row, new_id, clear) =
                    super::statuses_update_stages::rebuild_poll(store, status, existing, spec);
                plan.poll_write = Some(row);
                plan.poll_new_id = new_id;
                plan.poll_clear_votes = clear;
                plan.poll_job_due = Some(spec.expires_at_ms);
                plan.poll_changed = true;
            }
        }
    }

    if let Some(secs) = update.poll_expires_in_ms {
        // Extend the (possibly just-planned) poll without resetting votes;
        // ignored when the status has no poll.
        super::statuses_update_stages::extend_poll_expiry(&mut plan, old_poll.as_ref(), secs, now_ms);
    }

    Ok((old_poll, plan))
}

/// Apply the patch: pre-edit snapshot, media/poll/scalar/tag writes and the
/// post-edit snapshot.
/// Apply the patch: pre-edit snapshot, media/poll/scalar/tag writes and the
/// post-edit snapshot.
pub(super) fn apply_patch_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    status: &mut Status,
    account_id: Id,
    update: &StatusUpdate,
    new_media: &[Id],
    media_changed: bool,
    plan: &PollPlan,
    old_poll: Option<&Poll>,
    now_ms: u64,
) -> Result<()> {
    let id = status.id;

    let version_count = super::statuses_update_stages::snapshot_pre_edit_txn(
        write,
        status,
        account_id,
        old_poll,
    )?;

    // -- Media: detach rows leaving the list, attach rows joining it.
    if media_changed {
        let old_media = status.media_ids.clone();
        super::statuses_update_stages::reattach_media_txn(write, id, &old_media, new_media)?;
    }

    // -- Poll: mapping + ballot reset + row write + expiry job.
    super::statuses_update_stages::write_poll_side_txn(store, write, status, plan, now_ms)?;

    // -- Apply the scalar fields.
    super::statuses_update_stages::apply_scalar_fields(status, update, media_changed, new_media);
    // -- Tags: replace the stored list and move the hashtag-index keys
    // in this same transaction (bug 4: edits used to leave both stale).
    if let Some(new_tags) = &update.tags {
        super::statuses_update_stages::move_tag_index_txn(write, status, new_tags)?;
    }
    super::statuses_update_stages::apply_quote_policy(status, update);
    status.edited_at_ms = Some(now_ms);

    // -- Post-edit snapshot: version `count` (0 was just appended when the
    //    history was empty).
    super::statuses_update_stages::snapshot_post_edit_txn(
        write,
        status,
        account_id,
        version_count,
        now_ms,
    )?;

    write_status_txn(write, status)?;
    Ok(())
}
/// accepted-quoter fan-out, and the search-index job.
pub(super) fn enqueue_edit_jobs_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    status: &Status,
    now_ms: u64,
) -> Result<()> {
    let id = status.id;
    let account_id = status.account_id;

    super::federation_content::federation_event_txn(
        write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "update", "status_id": id.value()}),
        now_ms,
    )?;
    // -- D51 `update` (live rebloggers) + `quoted_update` (accepted
    // quoters' authors), same transaction (G-B producers).
    {
        use crate::application::notifications::{
            emit_quoted_update_for_quoters, emit_update_for_rebloggers,
        };
        let rebloggers: Vec<Id> =
            super::statuses_interactions::reblogged_by_impl(store, id, None, 5000)?
                .into_iter()
                .map(|(account, _)| account)
                .collect();
        for job in emit_update_for_rebloggers(account_id, id, &rebloggers, now_ms) {
            super::jobs::insert_job(
                write,
                &Job::new(store.next_id(), job.kind, job.payload, now_ms, now_ms),
            )?;
        }
        // Accepted quoters' authors, scanned inline on the held write
        // txn (`quotes_of_status` opens its own write txn and cannot
        // nest inside this one).
        let mut quoter_ids: Vec<Id> = Vec::new();
        {
            let by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
            let lower = schema::pair_key(id.value(), 0);
            let upper = schema::pair_key(id.value(), u64::MAX);
            for entry in by_quoted.range(lower.as_slice()..=upper.as_slice())? {
                let (key, value) = entry?;
                let bytes = key.value();
                let quote_id = u64::from_be_bytes(
                    bytes[8..]
                        .try_into()
                        .map_err(|_| Error::Storage("quote key is not 16 bytes".into()))?,
                );
                let quoting_status_id = Id::new(value.value());
                let Some(quote) = super::quotes::read_quote_txn(write, Id::new(quote_id))? else {
                    continue;
                };
                if !quote.is_accepted() {
                    continue;
                }
                let Some(quoting) = read_status_txn(write, quoting_status_id)? else {
                    continue;
                };
                if quoting.is_deleted() {
                    continue;
                }
                quoter_ids.push(quoting.account_id);
                if quoter_ids.len() >= 1000 {
                    break;
                }
            }
        }
        for job in emit_quoted_update_for_quoters(status.account_id, id, &quoter_ids, now_ms) {
            super::jobs::insert_job(
                write,
                &Job::new(store.next_id(), job.kind, job.payload, now_ms, now_ms),
            )?;
        }
    }
    // Phase 7.2: incremental search indexing rides the same commit.
    super::jobs::insert_job(
        write,
        &Job::new(
            store.next_id(),
            JobKind::SearchIndexStatus,
            crate::jobs::search_index::index_payload(id),
            now_ms,
            now_ms,
        ),
    )?;
    Ok(())
}

/// Apply an edit: snapshot the pre-edit version (once), patch fields, bump
/// `edited_at_ms`, keep counters — ONE transaction. Returns `Ok(None)`
/// when the patch changed nothing (no snapshot, no `edited_at` bump).
pub(super) fn update_status_impl(
    store: &RedbStore,
    id: Id,
    account_id: Id,
    update: StatusUpdate,
    now_ms: u64,
) -> Result<Option<Status>> {
    let write = store.begin_write()?;

    let mut status = read_status_txn(&write, id)?
        .filter(|s| !s.is_deleted() && s.account_id == account_id)
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // -- Media: validate the target list, then diff it against the old one.
    let old_media = status.media_ids.clone();
    let (new_media, media_changed) = diff_media(
        &write,
        account_id,
        id,
        update.media_ids.as_deref(),
        &old_media,
    )?;

    // -- Scalar fields: `Some(patch)` participates in the change check.
    let scalars = diff_scalars(&status, &update);

    // -- Poll plan (computed against the pre-edit row; applied only if
    //    something changed).
    let (old_poll, plan) = plan_poll(store, &write, &status, &update, now_ms)?;

    // -- Nothing effective changed: abort the (still read-only) txn so no
    //    snapshot, `edited_at` bump or write happens.
    let changed = scalars.source_changed
        || scalars.html_changed
        || scalars.spoiler_changed
        || scalars.sensitive_changed
        || scalars.language_changed
        || media_changed
        || scalars.tags_changed
        || scalars.place_changed
        || plan.poll_changed;
    if !changed {
        return Ok(None);
    }

    apply_patch_txn(
        store,
        &write,
        &mut status,
        account_id,
        &update,
        &new_media,
        media_changed,
        &plan,
        old_poll.as_ref(),
        now_ms,
    )?;
    enqueue_edit_jobs_txn(store, &write, &status, now_ms)?;
    write.commit()?;
    Ok(Some(status))
}

/// Rewrite a status's stored tag list and move its hashtag-index keys in
/// ONE transaction (D25 backfill writer).
pub(super) fn rewrite_status_tags_impl(
    store: &RedbStore,
    status_id: Id,
    new_tags: &[String],
) -> Result<()> {
    let write = store.begin_write()?;
    let old_tags: Vec<String> = {
        let table = write.open_table(super::schema::STATUSES)?;
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
        let mut table = write.open_table(super::schema::STATUSES)?;
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
