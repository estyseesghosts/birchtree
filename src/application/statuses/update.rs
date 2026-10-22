//! Status editing, deletion, and single-view reads.
//!
//! Missing, deleted and non-visible statuses are indistinguishable:
//! `NotFound("status")`, exactly like Mastodon's status sub-routes.

use super::create::PollCommand;
use super::create_mentions::canonical_tags;
use super::create_mentions::language_regex;
use super::create_mentions::render_html;
use super::create_validate::ERR_LANGUAGE;
use super::create_validate::ERR_RECORD_INVALID;
use super::create_validate::build_poll_spec;
use crate::application::policy;
use crate::application::ports::AccountStatusesQuery;
use crate::application::ports::DeletedStatus;
use crate::application::ports::PairState;
use crate::application::ports::StatusUpdate;
use crate::application::ports::Store;
use crate::domain::Error;
use crate::domain::Id;
use crate::domain::QuotePolicy;
use crate::domain::Result;
use crate::domain::Status;
use crate::domain::Visibility;

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
    if let Some(text) = &cmd.text {
        update.source_text = Some(text.clone());
        update.content_html = Some(render_html(text));
    }
    if let Some(spoiler) = cmd.spoiler_text {
        // Present-and-blank clears the CW (`Some(None)`, as in Mastodon).
        update.spoiler_text = Some(if spoiler.trim().is_empty() {
            None
        } else {
            Some(spoiler)
        });
    }
    if let Some(sensitive) = cmd.sensitive {
        update.sensitive = Some(sensitive);
    }
    if let Some(language) = cmd.language {
        if !language_regex().is_match(&language) {
            return Err(Error::InvalidInput(ERR_LANGUAGE.into()));
        }
        update.language = Some(language);
    }
    // Core extension CORE-P: same place gate as creation (`None` leaves
    // the stored place unchanged; independent of media edits). Replies
    // and DMs exempt.
    if let Some(raw_place) = place_id {
        let resolves = match raw_place {
            None => false,
            Some(id) => store.place_by_id(id)?.is_some(),
        };
        update.place_id = Some(crate::application::place_policy::check_place_policy(
            place_policy,
            status.visibility == Visibility::Direct,
            status.in_reply_to_id.is_some(),
            raw_place,
            resolves,
        )?);
    }
    if let Some(media_ids) = cmd.media_ids {
        // Core extension CORE-L: same enforced gate as creation, evaluated
        // against the resulting attachment set. Replies and DMs exempt.
        if license_policy == "enforced"
            && status.in_reply_to_id.is_none()
            && status.visibility != Visibility::Direct
            && !media_ids.is_empty()
        {
            let attached = store.media_by_ids(&media_ids)?;
            let author = store
                .account_by_id(cmd.account_id)?
                .ok_or_else(|| Error::NotFound("account".into()))?;
            let mut effective = Vec::with_capacity(media_ids.len());
            for id in &media_ids {
                if let Some(media) = attached.get(id) {
                    effective.push(crate::application::license_policy::effective_license(
                        media.license,
                        author.default_license,
                    ));
                }
            }
            if effective.len() == media_ids.len() {
                crate::application::license_policy::check_status_licenses(
                    license_policy,
                    false,
                    false,
                    true,
                    &effective,
                )?;
            }
        }
        update.media_ids = Some(media_ids);
    }
    if let Some(poll) = cmd.poll {
        update.poll = Some(build_poll_spec(now_ms, poll)?);
    }
    if let Some(raw) = cmd.quote_approval_policy {
        update.quote_approval_policy = Some(
            QuotePolicy::from_wire(&raw)
                .ok_or_else(|| Error::InvalidInput(ERR_RECORD_INVALID.into()))?,
        );
    }
    // Core extension CORE-T: same block gate as creation, evaluated
    // against the resulting attachment set (patched or stored). Replies
    // are NOT exempt here (the rule covers every write leaving an
    // undescribed image/video); text-only edits pass.
    if matches!(alt_text_policy, "nag" | "block") {
        let resulting: Vec<Id> = match &update.media_ids {
            Some(ids) => ids.clone(),
            None => status.media_ids.clone(),
        };
        if !resulting.is_empty() {
            let attached = store.media_by_ids(&resulting)?;
            let mut pairs = Vec::with_capacity(resulting.len());
            for id in &resulting {
                if let Some(media) = attached.get(id) {
                    pairs.push((media.r#type.clone(), media.description.clone()));
                }
            }
            if pairs.len() == resulting.len() {
                crate::application::alt_text_policy::check_block(alt_text_policy, &pairs)?;
            }
        }
    }
    // Tags are re-derived on every edit (steady-state write path): the
    // effective text is the edited text, or the stored text when the edit
    // leaves it alone. The store replaces the tag list and moves the
    // hashtag-index keys in the same transaction.
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
