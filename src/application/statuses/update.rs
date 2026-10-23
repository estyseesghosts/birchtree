//! Status editing, deletion, and single-view reads.
//!
//! Missing, deleted and non-visible statuses are indistinguishable:
//! `NotFound("status")`, exactly like Mastodon's status sub-routes.

use super::create::PollCommand;
use super::create_mentions::canonical_tags;
use super::update_stages;
use crate::application::policy;
use crate::application::ports::AccountStatusesQuery;
use crate::application::ports::DeletedStatus;
use crate::application::ports::PairState;
use crate::application::ports::StatusUpdate;
use crate::application::ports::Store;
use crate::domain::Error;
use crate::domain::Id;
use crate::domain::Result;
use crate::domain::Status;

/// Input for `PUT /api/v1/statuses/:id`.
///
/// `None` fields are left unchanged.
#[derive(Debug, Clone)]
pub struct UpdateStatusCommand {
    pub account_id: Id,
    pub status_id: Id,
    pub text: Option<String>,
    pub sensitive: Option<bool>,
    pub spoiler_text: Option<String>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<Id>>,
    pub poll: Option<PollCommand>,
    /// Wire value, validated here when present.
    pub quote_approval_policy: Option<String>,
}

/// `PUT /api/v1/statuses/:id` — edit an owned status.
///
/// Missing, deleted and non-owned statuses are indistinguishable
/// (`NotFound("status")`, matching Mastodon's `Status.where(account:).
/// find`). Returns `Ok(None)` when the patch changed nothing (no edit
/// snapshot, no `edited_at` bump). Note the deliberate Mastodon-shaped
/// asymmetries: an unknown `language` or `quote_approval_policy` is a 422
/// while text length/blankness are re-validated by the store-side rules only
/// at creation; a supplied poll is validated exactly as on creation.
pub fn update_status(
    store: &dyn Store,
    now_ms: u64,
    cmd: UpdateStatusCommand,
    license_policy: &str,
    place_policy: &str,
    place_id: Option<Option<Id>>,
    alt_text_policy: &str,
) -> Result<Option<Status>> {
    let status = store
        .status_by_id(cmd.status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    if status.is_deleted() || !policy::is_owner(cmd.account_id, &status) {
        return Err(Error::NotFound("status".into()));
    }

    let mut update = StatusUpdate::default();
    update_stages::apply_text(&cmd.text, &mut update);
    update_stages::apply_spoiler(cmd.spoiler_text, &mut update);
    update_stages::apply_language(cmd.language, &mut update)?;
    update.place_id = update_stages::resolve_place(store, place_policy, &status, place_id)?;
    update.media_ids =
        update_stages::resolve_media(store, license_policy, &status, cmd.account_id, cmd.media_ids)?;
    update_stages::apply_poll(cmd.poll, now_ms, &mut update)?;
    update_stages::apply_quote_policy(cmd.quote_approval_policy, &mut update)?;
    update_stages::check_alt_text(store, alt_text_policy, &status, &update)?;
    // Tags are re-derived on every edit (steady-state write path): the
    // effective text is the edited text, or the stored text when unchanged.
    let effective_text = cmd
        .text
        .clone()
        .unwrap_or_else(|| status.source_text.clone());
    update.tags = Some(canonical_tags(&effective_text));

    store.update_status(cmd.status_id, cmd.account_id, update, now_ms)
}

/// `DELETE /api/v1/statuses/:id` — tombstone a status, returning the
/// pre-deletion record. Ownership lives in the store: missing, deleted and
/// non-owned all answer `NotFound`.
pub fn delete_status(
    store: &dyn Store,
    viewer: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<DeletedStatus> {
    store.delete_status(status_id, viewer, now_ms)
}

/// `GET /api/v1/statuses/:id` — one status under the show gate.
///
/// Missing, deleted and non-visible statuses are indistinguishable:
/// `NotFound("status")`, exactly like Mastodon's status sub-routes.
pub fn show_status(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let status = store
        .status_by_id(status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    if !fetchable(store, viewer, &status, now_ms)? {
        return Err(Error::NotFound("status".into()));
    }
    Ok(status)
}

/// `GET /api/v1/statuses/:id/source` — the raw source of a status under the
/// same visibility gate as [`show_status`].
pub fn status_source(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    show_status(store, viewer, status_id, now_ms)
}

/// Mastodon's multi-fetch (`GET /api/v1/statuses?ids[]=`): non-deleted
/// statuses among `ids`, id-descending, silently omitting anything the
/// viewer may not fetch.
pub fn statuses_by_ids(
    store: &dyn Store,
    viewer: Option<Id>,
    ids: &[Id],
    now_ms: u64,
) -> Result<Vec<Status>> {
    // Mastodon deduplicates the requested ids (`status_ids` = `.uniq`).
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let rows = store.statuses_by_ids(&ids)?;
    let mut visible = Vec::with_capacity(rows.len());
    for status in rows {
        if fetchable(store, viewer, &status, now_ms)? {
            visible.push(status);
        }
    }
    Ok(visible)
}

/// `GET /api/v1/accounts/:id/statuses` — one account's statuses under the
/// query's cursors/filters, then per-viewer filtering.
///
/// A missing account is `NotFound` (404). An anonymous viewer sees only
/// distributable statuses: `can_fetch` with the default `PairState` admits
/// `public`/`unlisted` and rejects `private`/`direct`.
pub fn account_statuses(
    store: &dyn Store,
    viewer: Option<Id>,
    account_id: Id,
    query: AccountStatusesQuery,
    now_ms: u64,
) -> Result<Vec<Status>> {
    if store.account_by_id(account_id)?.is_none() {
        return Err(Error::NotFound("status".into()));
    }
    let rows = store.statuses_by_account(account_id, query)?;
    let mut visible = Vec::with_capacity(rows.len());
    for status in rows {
        if fetchable(store, viewer, &status, now_ms)? {
            visible.push(status);
        }
    }
    Ok(visible)
}

/// `can_fetch(viewer, status, pair_state(author, viewer))` for one status.
pub(crate) fn fetchable(
    store: &dyn Store,
    viewer: Option<Id>,
    status: &Status,
    now_ms: u64,
) -> Result<bool> {
    let pair = match viewer {
        Some(viewer) => store.pair_state(status.account_id, viewer, now_ms)?,
        None => PairState::default(),
    };
    Ok(policy::can_fetch(viewer, status, pair))
}
