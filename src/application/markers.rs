//! Timeline marker use cases (`GET`/`POST /api/v1/markers`).
//!
//! Mastodon's `Marker` rows carry an optimistic `lock_version`: a stale
//! update answers 409 `{"error":"Conflict during update, please try again"}`.
//! Scopes are `read`/`write:statuses` (not `read:notifications`), matching
//! the pinned controller.

use crate::application::ports::{MarkerUpdateError, Store};
use crate::domain::{Error, Id, Marker, Result};

/// `GET /api/v1/markers?timeline[]=home&timeline[]=notifications` — only the
/// requested timelines that exist are returned.
pub fn index(store: &dyn Store, account_id: Id, timelines: &[&str]) -> Result<Vec<Marker>> {
    let requested: Vec<&str> = timelines
        .iter()
        .copied()
        .filter(|t| Marker::timeline_valid(t))
        .collect();
    store.markers(account_id, &requested)
}

/// `POST /api/v1/markers` — upsert `home[last_read_id]` /
/// `notifications[last_read_id]` in one call.
pub fn update(
    store: &dyn Store,
    account_id: Id,
    updates: &[(String, Id, Option<u64>)],
    now_ms: u64,
) -> Result<Vec<Marker>> {
    let mut out = Vec::with_capacity(updates.len());
    for (timeline, last_read_id, lock_version) in updates {
        let marker = store
            .set_marker(account_id, timeline, *last_read_id, *lock_version, now_ms)
            .map_err(|err| match err {
                MarkerUpdateError::Stale => {
                    Error::Conflict("Conflict during update, please try again".into())
                }
                MarkerUpdateError::InvalidTimeline => {
                    Error::InvalidInput(format!("Timeline is not included in the list: {timeline}"))
                }
            })?;
        out.push(marker);
    }
    Ok(out)
}
