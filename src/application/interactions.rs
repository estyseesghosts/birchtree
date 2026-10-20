//! Status interactions: favourites, bookmarks, reblogs, pins.
//!
//! Each use case runs the shared visibility/interaction policy first — a
//! status sub-route answers 404 for anything the viewer may not fetch — then
//! delegates the atomic row + counter + follow-up-job write to the
//! `StatusStore` port. Policy predicates are reused from `application::policy`,
//! never re-implemented here.

use crate::application::ports::{PairState, Store};
use crate::application::policy;
use crate::domain::{Error, Id, InteractionEvent, InteractionEventKind, Result, Status, Visibility};

/// Load `status_id` as `viewer_id` under the show gate.
///
/// Missing, deleted and otherwise non-visible statuses are indistinguishable
/// (`NotFound("status")`, as in Mastodon's status sub-routes). The full
/// show-equivalent gate ([`policy::can_fetch_full`]) also refuses when the
/// viewer blocks/mutes the author or either side domain-blocks the other
/// (F2). Returns the status together with the viewer/author pair state for
/// interaction gates.
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
    let facts = policy::viewer_side_facts(store, viewer_id, status.account_id, now_ms)?;
    if !policy::can_fetch_full(Some(viewer_id), &status, pair, facts) {
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
    // D39 journal: upstream counts only the create (`favourite_service.rb`
    // returns early on an existing row). No newness signal comes back from
    // the port, so pre-check the viewer flags; a same-instant double-tap can
    // over-count by one, the same race upstream's check-then-create has.
    let fresh = store
        .viewer_status_flags(viewer_id, &[status.id])
        .map(|flags| !flags.get(&status.id).is_some_and(|f| f.favourited))
        .unwrap_or(true);
    let status = store.favourite_status(viewer_id, status.id, now_ms)?;
    if fresh {
        let _ = store.record_interaction(InteractionEvent::for_journal(
            viewer_id,
            InteractionEventKind::Favourite,
            now_ms,
        ));
    }
    Ok(status)
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
/// Visibility follows `ReblogService` (`app/services/reblog_service.rb:23-26`
/// at pin `8ff11c2b`): a non-distributable (hidden) target keeps the
/// target's visibility even over an explicit param; otherwise the explicit
/// param wins, falling back to the viewer's `default_privacy` (explicit >
/// user default > locked-account `private` > `public`, the same chain as
/// status creation), and reblogs may never be `direct`.
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
    let visibility = if !target.visibility.is_distributable() {
        target.visibility
    } else {
        match visibility {
            Some(raw) => Visibility::from_wire(raw)
                .filter(|value| value.is_rebloggable())
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "Validation failed: Visibility must be public, unlisted or private".into(),
                    )
                })?,
            None => default_reblog_visibility(store, viewer_id)?,
        }
    };
    // D39 journal, same newness rule as favourite (upstream
    // `reblog_service.rb` counts only the create).
    let fresh = store
        .viewer_status_flags(viewer_id, &[target.id])
        .map(|flags| !flags.get(&target.id).is_some_and(|f| f.reblogged))
        .unwrap_or(true);
    let reblog = store.reblog_status(viewer_id, target.id, visibility, now_ms)?;
    if fresh {
        let _ = store.record_interaction(InteractionEvent::for_journal(
            viewer_id,
            InteractionEventKind::Reblog,
            now_ms,
        ));
    }
    Ok(reblog)
}

/// The reblog visibility when no explicit value is passed: the viewer's
/// stored `default_privacy`, falling back to `private` for locked accounts
/// and `public` otherwise — the same chain as status creation
/// (`PostStatusService` `@options[:visibility] || @account.user&.setting_default_privacy`
/// with `setting_default_privacy = settings['default_privacy'] || locked? private : public`).
/// A corrupt stored value falls back to the locked-rule default rather than
/// failing the reblog.
fn default_reblog_visibility(store: &dyn Store, viewer_id: Id) -> Result<Visibility> {
    let viewer = store
        .account_by_id(viewer_id)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
    let stored = store
        .user_by_account_id(viewer_id)?
        .and_then(|user| user.default_privacy);
    let raw = stored.unwrap_or_else(|| {
        if viewer.locked {
            "private".to_string()
        } else {
            "public".to_string()
        }
    });
    Ok(Visibility::from_wire(&raw).unwrap_or(if viewer.locked {
        Visibility::Private
    } else {
        Visibility::Public
    }))
}

/// `POST /api/v1/statuses/:id/unreblog` — deletes the viewer's reblog of the
/// (unwrapped) target and returns the ORIGINAL; 200 no-op when the viewer
/// never reblogged it.
///
/// The destroy path carries NO reblog gate (upstream `reblogs_controller.rb`
/// `destroy` at pin `8ff11c2b` authorizes `:unreblog?`/`:show?` only, never
/// `:reblog?`): an existing reblog is deleted even when the target is no
/// longer rebloggable by the viewer. Only the no-op path show-gates the
/// target before returning the original.
pub fn unreblog(store: &dyn Store, viewer_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let status = store
        .status_by_id(status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let target = reblog_target(store, status)?;
    let reblogged = store
        .viewer_status_flags(viewer_id, &[target.id])
        .map(|flags| flags.get(&target.id).is_some_and(|f| f.reblogged))
        .unwrap_or(false);
    if reblogged {
        return store.unreblog_status(viewer_id, target.id, now_ms);
    }
    let _ = fetch_visible(store, viewer_id, target.id, now_ms)?;
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
