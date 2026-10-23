//! Fine-grained stages for the status-edit transaction.
//!
//! `update_status_impl` (in `statuses_update.rs`) opens ONE `redb` write
//! transaction; `plan_poll` and `apply_patch_txn` are its in-transaction
//! fragments (not single-transaction bodies themselves, so they cannot be
//! grandfathered). This module splits each fragment into named stage
//! functions; the slimmed originals keep the transaction boundary and call
//! these stages in the same order. No writes move across the boundary:
//!
//! * pure stages (`adjust_poll_in_place`, `apply_scalar_fields`,
//!   `apply_quote_policy`) take no transaction and no store;
//! * `rebuild_poll` / `write_poll_side_txn` take `&RedbStore` only to mint
//!   generated ids via `next_id` (no tables opened outside the txn);
//! * every other stage takes `&WriteTransaction` first and runs inside the
//!   caller's transaction.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewPollSpec, StatusUpdate};
use crate::domain::{Error, Id, Job, JobKind, Poll, PollOption, QuotePolicy, Result, Status};
use crate::domain::{StatusVersion, Visibility};

use super::media::{read_media_txn, write_media_txn};
use super::schema::{self, HASHTAG_INDEX, POLL_BY_STATUS, POLLS, STATUS_VERSIONS};
use super::statuses_delete::clear_poll_votes;
use super::statuses_update::{PollPlan, insert_version, read_poll_txn};
use super::{RedbStore, encode};

/// Whether an edit poll spec matches the stored poll's options shape.
///
/// Moved verbatim from `statuses_update.rs` (only `plan_poll` used it);
/// `pub(super)` so the slimmed `plan_poll` guard can call it.
pub(super) fn same_shape_poll(existing: &Poll, spec: &NewPollSpec) -> bool {
    existing.multiple == spec.multiple
        && existing.options.len() == spec.options.len()
        && existing
            .options
            .iter()
            .zip(spec.options.iter())
            .all(|(option, title)| option.title == *title)
}

/// Same-shape poll edit: adjust expiry/flags in place (no vote reset).
/// Returns the rewritten row and its new expiry due time, or `None` when
/// nothing changed.
pub(super) fn adjust_poll_in_place(old: &Poll, spec: &NewPollSpec) -> Option<(Poll, u64)> {
    if !same_shape_poll(old, spec) {
        return None;
    }
    let mut updated = old.clone();
    let mut touched = false;
    if updated.hide_totals != spec.hide_totals {
        updated.hide_totals = spec.hide_totals;
        touched = true;
    }
    if updated.expires_at_ms != Some(spec.expires_at_ms) {
        updated.expires_at_ms = Some(spec.expires_at_ms);
        touched = true;
    }
    if touched {
        Some((updated, spec.expires_at_ms))
    } else {
        None
    }
}

/// Rebuild the poll row with all tallies reset (Mastodon resets votes on
/// option edits), or mint a fresh row when the status had no poll.
///
/// Returns the row to upsert, the freshly minted poll id (needs a
/// `POLL_BY_STATUS` mapping), and the poll whose ballots must reset first.
/// `store` is used only to mint the generated id.
pub(super) fn rebuild_poll(
    store: &RedbStore,
    status: &Status,
    existing: Option<&Poll>,
    spec: &NewPollSpec,
) -> (Poll, Option<Id>, Option<Id>) {
    // No poll yet, or options/multiple changed: rebuild with all
    // tallies reset (Mastodon resets votes on option edits).
    let (mut rebuilt, new_id, clear_votes) = match existing {
        Some(current) => (current.clone(), None, Some(current.id)),
        None => {
            let pid = store.next_id();
            (
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
                },
                Some(pid),
                None,
            )
        }
    };
    rebuilt.multiple = spec.multiple;
    rebuilt.hide_totals = spec.hide_totals;
    rebuilt.expires_at_ms = Some(spec.expires_at_ms);
    rebuilt.expired = false;
    rebuilt.options = spec
        .options
        .iter()
        .map(|title| PollOption {
            title: title.clone(),
            votes_count: 0,
        })
        .collect();
    rebuilt.voters_count = 0;
    rebuilt.votes_count = 0;
    (rebuilt, new_id, clear_votes)
}

/// Extend the (possibly just-planned) poll without resetting votes; ignored
/// when the status has no poll. Pure in-memory plan edit.
pub(super) fn extend_poll_expiry(
    plan: &mut PollPlan,
    old_poll: Option<&Poll>,
    secs: u64,
    now_ms: u64,
) {
    let current = plan.poll_write.take().or_else(|| old_poll.cloned());
    if let Some(mut current) = current {
        let new_expiry = now_ms.saturating_add(secs.saturating_mul(1000));
        if current.expires_at_ms != Some(new_expiry) {
            current.expires_at_ms = Some(new_expiry);
            plan.poll_job_due = Some(new_expiry);
            plan.poll_changed = true;
        }
        plan.poll_write = Some(current);
    }
}

/// Record the pre-edit state once as version 0 and return the total version
/// count (the post-edit snapshot uses it as its version number).
pub(super) fn snapshot_pre_edit_txn(
    write: &WriteTransaction,
    status: &Status,
    account_id: Id,
    old_poll: Option<&Poll>,
) -> Result<u64> {
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
    Ok(version_count)
}

/// Detach rows leaving the media list and attach rows joining it.
pub(super) fn reattach_media_txn(
    write: &WriteTransaction,
    status_id: Id,
    old_media: &[Id],
    new_media: &[Id],
) -> Result<()> {
    for mid in old_media {
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
            media.status_id = Some(status_id);
            write_media_txn(write, &media)?;
        }
    }
    Ok(())
}

/// Poll mapping, ballot reset, row write and expiry job. `store` is used
/// only to mint generated job ids.
pub(super) fn write_poll_side_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    status: &mut Status,
    plan: &PollPlan,
    now_ms: u64,
) -> Result<()> {
    if let Some(pid) = plan.poll_new_id {
        let mut by_status = write.open_table(POLL_BY_STATUS)?;
        by_status.insert(status.id.value(), pid.value())?;
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
    Ok(())
}

/// Apply the scalar patch fields (pure in-memory edit of the status row).
pub(super) fn apply_scalar_fields(
    status: &mut Status,
    update: &StatusUpdate,
    media_changed: bool,
    new_media: &[Id],
) {
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
}

/// Replace the stored tag list and move the hashtag-index keys (this same
/// transaction; bug 4: edits used to leave both stale).
pub(super) fn move_tag_index_txn(
    write: &WriteTransaction,
    status: &mut Status,
    new_tags: &Vec<String>,
) -> Result<()> {
    if *new_tags != status.tags {
        let mut index = write.open_table(HASHTAG_INDEX)?;
        for tag in &status.tags {
            if !new_tags.contains(tag) {
                index.remove(schema::hashtag_key(tag, status.id.value()).as_slice())?;
            }
        }
        for tag in new_tags {
            if !status.tags.contains(tag) {
                index.insert(
                    schema::hashtag_key(tag, status.id.value()).as_slice(),
                    status.id.value(),
                )?;
            }
        }
        status.tags = new_tags.clone();
    }
    Ok(())
}

/// `quote_approval_policy` rides along with a real edit but is not a change
/// by itself (no snapshot, no `edited_at`); the dedicated `set_quote_policy`
/// endpoint owns standalone policy updates.
pub(super) fn apply_quote_policy(status: &mut Status, update: &StatusUpdate) {
    if let Some(policy) = update.quote_approval_policy {
        status.quote_policy = match status.visibility {
            Visibility::Private | Visibility::Direct => QuotePolicy::Nobody,
            _ => policy,
        };
    }
}

/// Post-edit snapshot: version `count` (0 was just appended when the history
/// was empty).
pub(super) fn snapshot_post_edit_txn(
    write: &WriteTransaction,
    status: &Status,
    account_id: Id,
    version_count: u64,
    now_ms: u64,
) -> Result<()> {
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
        status_id: status.id,
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
    Ok(())
}
