//! Status interactions: favourites, bookmarks, reblogs, pins.
//!
//! Each use case runs the shared visibility/interaction policy first — a
//! status sub-route answers 404 for anything the viewer may not fetch — then
//! delegates the atomic row + counter + follow-up-job write to the
//! `StatusStore` port. Policy predicates are reused from `application::policy`,
//! never re-implemented here.

use crate::application::ports::{PairState, Store};
use crate::application::policy;
use crate::domain::{Error, Id, Result, Status, Visibility};

/// Load `status_id` as `viewer_id` under the show gate.
///
/// Missing, deleted and otherwise non-visible statuses are indistinguishable
/// (`NotFound("status")`, as in Mastodon's status sub-routes). Returns the
/// status together with the viewer/author pair state for interaction gates.
fn fetch_visible(
    store: &dyn Store,
    viewer_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<(Status, PairState)> {
    let status = store
        .status_by_id(status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let pair = store.pair_state(status.account_id, viewer_id, now_ms)?;
    if !policy::can_fetch(Some(viewer_id), &status, pair) {
        return Err(Error::NotFound("status".into()));
    }
    Ok((status, pair))
}

/// Reblog operations always act on the original: unwrap a reblog wrapper to
/// its target (loading it afresh), or pass the status through unchanged.
/// A vanished/tombstoned target is `NotFound("status")`.
fn reblog_target(store: &dyn Store, status: Status) -> Result<Status> {
    match status.reblog_of_id {
        Some(target_id) => store
            .status_by_id(target_id)?
            .filter(|target| !target.is_deleted())
            .ok_or_else(|| Error::NotFound("status".into())),
        None => Ok(status),
    }
}

/// `POST /api/v1/statuses/:id/favourite` — idempotent: already-favourited is
/// returned unchanged, with updated counters from the store transaction.
pub fn favourite(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let (status, pair) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    if !policy::can_favourite(viewer_id, &status, pair) {
        return Err(Error::NotFound("status".into()));
    }
    store.favourite_status(viewer_id, status.id, now_ms)
}

/// `POST /api/v1/statuses/:id/unfavourite` — 200 no-op when the viewer never
/// favourited the status; missing/invisible statuses still 404.
pub fn unfavourite(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let (status, _) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    store.unfavourite_status(viewer_id, status.id, now_ms)
}

/// `POST /api/v1/statuses/:id/bookmark` — idempotent.
pub fn bookmark(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let (status, pair) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    if !policy::can_bookmark(viewer_id, &status, pair) {
        return Err(Error::NotFound("status".into()));
    }
    store.bookmark_status(viewer_id, status.id, now_ms)
}

/// `POST /api/v1/statuses/:id/unbookmark` — 200 no-op when not bookmarked.
pub fn unbookmark(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let (status, _) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    store.unbookmark_status(viewer_id, status.id, now_ms)
}

/// `POST /api/v1/statuses/:id/reblog` — returns the NEW reblog record.
///
/// Reblogging an existing reblog operates on its target (Mastodon unwraps).
/// `visibility` defaults to `public`; reblogs may never be `direct`.
pub fn reblog(
    store: &dyn Store,
    viewer_id: Id,
    status_id: Id,
    visibility: Option<&str>,
    now_ms: u64,
) -> Result<Status> {
    let (status, _) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    let target = reblog_target(store, status)?;
    let pair = store.pair_state(target.account_id, viewer_id, now_ms)?;
    if !policy::can_reblog(viewer_id, &target, pair) {
        return Err(Error::Forbidden("This action is not allowed".into()));
    }
    let visibility = match visibility {
        None => Visibility::Public,
        Some(raw) => Visibility::from_wire(raw)
            .filter(|value| value.is_rebloggable())
            .ok_or_else(|| {
                Error::InvalidInput(
                    "Validation failed: Visibility must be public, unlisted or private".into(),
                )
            })?,
    };
    store.reblog_status(viewer_id, target.id, visibility, now_ms)
}

/// `POST /api/v1/statuses/:id/unreblog` — deletes the viewer's reblog of the
/// (unwrapped) target and returns the ORIGINAL; 200 no-op when the viewer
/// never reblogged it.
pub fn unreblog(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let (status, _) = fetch_visible(store, viewer_id, status_id, now_ms)?;
    let target = reblog_target(store, status)?;
    let pair = store.pair_state(target.account_id, viewer_id, now_ms)?;
    if !policy::can_reblog(viewer_id, &target, pair) {
        return Err(Error::Forbidden("This action is not allowed".into()));
    }
    store.unreblog_status(viewer_id, target.id, now_ms)
}

/// `POST /api/v1/statuses/:id/pin`.
///
/// Only missing/deleted statuses 404 here: ownership, boost/direct/foreign
/// and pin-count validation live in the store and surface unchanged as 422
/// (`Validation failed: …`).
pub fn pin(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    store
        .status_by_id(status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("status".into()))?;
    store.pin_status(viewer_id, status_id, now_ms)
}

/// `POST /api/v1/statuses/:id/unpin` — 200 no-op when the status is not
/// pinned; same 404/422 split as [`pin`].
pub fn unpin(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    store
        .status_by_id(status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("status".into()))?;
    store.unpin_status(viewer_id, status_id, now_ms)
}
