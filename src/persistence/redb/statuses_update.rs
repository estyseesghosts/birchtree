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

use crate::application::ports::{NewPollSpec, StatusUpdate};
use crate::domain::{
    Error, Id, Job, JobKind, Poll, PollOption, QuotePolicy, Result, Status, StatusVersion,
    Visibility,
};

use super::media::{check_attachable_txn, read_media_txn, write_media_txn};
use super::schema::{
    self, HASHTAG_INDEX, POLL_BY_STATUS, POLLS, QUOTES_BY_QUOTED, STATUS_VERSIONS,
};
use super::statuses_delete::clear_poll_votes;
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
    poll_write: Option<Poll>,
    /// Freshly minted poll id (needs a `POLL_BY_STATUS` mapping).
    poll_new_id: Option<Id>,
    /// Poll whose ballots must be reset first.
    poll_clear_votes: Option<Id>,
    /// `PollExpired` due time.
    poll_job_due: Option<u64>,
    poll_changed: bool,
}

/// Treat `Some("")` and `None` as the same (empty) spoiler.
fn norm_spoiler_text(spoiler: &Option<String>) -> Option<&str> {
    spoiler.as_deref().filter(|v| !v.is_empty())
}

/// Whether an edit poll spec matches the stored poll's options shape.
fn same_shape_poll(existing: &Poll, spec: &NewPollSpec) -> bool {
    existing.multiple == spec.multiple
        && existing.options.len() == spec.options.len()
        && existing
            .options
            .iter()
            .zip(spec.options.iter())
            .all(|(option, title)| option.title == *title)
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
        let same_shape = old_poll.as_ref().is_some_and(|p| same_shape_poll(p, spec));
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
                    plan.poll_write = Some(p);
                    plan.poll_job_due = Some(spec.expires_at_ms);
                    plan.poll_changed = true;
                }
            }
            // No poll yet, or options/multiple changed: rebuild with all
            // tallies reset (Mastodon resets votes on option edits).
            existing => {
                let mut p = match existing {
                    Some(p) => {
                        plan.poll_clear_votes = Some(p.id);
                        p.clone()
                    }
                    None => {
                        let pid = store.next_id();
                        plan.poll_new_id = Some(pid);
                        Poll {
                            id: pid,
                            status_id: status.id,
                            account_id: status.account_id,
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
                plan.poll_write = Some(p);
                plan.poll_job_due = Some(spec.expires_at_ms);
                plan.poll_changed = true;
            }
        }
    }

    if let Some(secs) = update.poll_expires_in_ms {
        // Extend the (possibly just-planned) poll without resetting votes;
        // ignored when the status has no poll.
        let current = plan.poll_write.take().or_else(|| old_poll.clone());
        if let Some(mut p) = current {
            let new_expiry = now_ms.saturating_add(secs.saturating_mul(1000));
            if p.expires_at_ms != Some(new_expiry) {
                p.expires_at_ms = Some(new_expiry);
                plan.poll_job_due = Some(new_expiry);
                plan.poll_changed = true;
            }
            plan.poll_write = Some(p);
        }
    }

    Ok((old_poll, plan))
}

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
            poll_options: old_poll.map(|p| {
                p.options
                    .iter()
                    .map(|option| option.title.clone())
                    .collect()
            }),
            created_at_ms: status.created_at_ms,
        };
        insert_version(write, &pre_edit)?;
        version_count = 1;
    }

    // -- Media: detach rows leaving the list, attach rows joining it.
    if media_changed {
        let old_media = status.media_ids.clone();
        for mid in &old_media {
            if !new_media.contains(mid)
                && let Some(mut media) = read_media_txn(write, *mid)?
            {
                media.status_id = None;
                write_media_txn(write, &media)?;
            }
        }
        for mid in new_media {
            if !old_media.contains(mid) {
                let Some(mut media) = read_media_txn(write, *mid)? else {
                    return Err(Error::InvalidInput(format!(
                        "Media {mid} not found or already attached to another post"
                    )));
                };
                media.status_id = Some(id);
                write_media_txn(write, &media)?;
            }
        }
    }

    // -- Poll: mapping + ballot reset + row write + expiry job.
    if let Some(pid) = plan.poll_new_id {
        let mut by_status = write.open_table(POLL_BY_STATUS)?;
        by_status.insert(id.value(), pid.value())?;
    }
    if let Some(pid) = plan.poll_clear_votes {
        clear_poll_votes(write, pid)?;
    }
    if let Some(poll) = &plan.poll_write {
        let mut polls = write.open_table(POLLS)?;
        polls.insert(poll.id.value(), encode(poll)?.as_slice())?;
    }
    if let Some(pid) = plan.poll_new_id {
        status.poll_id = Some(pid);
    }
    if let (Some(due), Some(poll)) = (plan.poll_job_due, &plan.poll_write) {
        let job = Job::new(
            store.next_id(),
            JobKind::PollExpired,
            serde_json::json!({ "poll_id": poll.id.value() }),
            due,
            now_ms,
        );
        super::jobs::insert_job(write, &job)?;
    }

    // -- Apply the scalar fields.
    if let Some(v) = &update.source_text {
        status.source_text = v.clone();
    }
    if let Some(v) = &update.content_html {
        status.content_html = v.clone();
    }
    if update.spoiler_text.is_some() {
        status.spoiler_text = update
            .spoiler_text
            .clone()
            .flatten()
            .filter(|s| !s.is_empty());
    }
    if let Some(v) = update.sensitive {
        status.sensitive = v;
    }
    if let Some(v) = &update.language {
        status.language = Some(v.clone());
    }
    if media_changed {
        status.media_ids = new_media.to_vec();
    }
    if let Some(place) = update.place_id {
        status.place_id = place;
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
                    index.insert(schema::hashtag_key(tag, id.value()).as_slice(), id.value())?;
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
        Some(pid) => read_poll_txn(write, pid)?.map(|p| {
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
    insert_version(write, &snapshot)?;

    write_status_txn(write, status)?;
    Ok(())
}

/// Enqueue the edit's follow-up jobs: federation event, live-reblogger and
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
