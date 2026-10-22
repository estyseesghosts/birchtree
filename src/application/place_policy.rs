//! Place-attachment policy (core extension, Stage 1 CORE-P).
//!
//! Pure function over `Config.place_policy` (`"off" | "unenforced" |
//! "enforced"`, default `"off"`). No DB access: the caller resolves
//! `place_id` to `place_resolves` (registry hit) before calling, and persists
//! the returned id on the status.
//!
//! Proposed wiring (not applied): `CreateStatusCommand` /
//! `UpdateStatusCommand` gain `place_id: Option<Id>`; `create_status` /
//! `update_status` in `src/application/statuses.rs` call
//! [`check_place_policy`] after reply resolution and store the returned id as
//! `Status.place_id` (proposed field on `src/domain/status.rs`).
//!
//! Wire-up is proposed (not applied): add `pub mod place_policy;` to
//! `src/application/mod.rs`.

use crate::domain::{Error, Id, Result};

/// Apply the place policy to one status write.
///
/// - `policy`: `"off" | "unenforced" | "enforced"` (anything else is a
///   server misconfiguration → `Internal`, never a client 422).
/// - `visibility_is_direct`: true for direct-visibility (DM) statuses, which
///   are always exempt.
/// - `is_reply`: true when the status is a reply (`in_reply_to_id.is_some()`),
///   which is always exempt.
/// - `place_id`: the requested place attachment, if any.
/// - `place_resolves`: whether `place_id` names a known registry row.
///   Ignored when `place_id` is `None`.
///
/// Returns the place id to persist (`None` = stored NULL):
///
/// - `off` → always `Ok(None)` (accepted-and-ignored).
/// - `unenforced` → `Ok(place_id)` when `None` or resolvable;
///   `Err(InvalidInput)` when `Some` but unresolvable.
/// - `enforced` → like `unenforced`, except a top-level non-DM status with
///   `None` or an unresolvable id is `Err(InvalidInput)`. Replies and DMs are
///   exempt (same as `unenforced`).
pub fn check_place_policy(
    policy: &str,
    visibility_is_direct: bool,
    is_reply: bool,
    place_id: Option<Id>,
    place_resolves: bool,
) -> Result<Option<Id>> {
    match policy {
        "off" => Ok(None),
        "unenforced" => {
            match place_id {
                None => Ok(None),
                Some(id) if place_resolves => Ok(Some(id)),
                Some(_) => Err(Error::InvalidInput(
                    "Validation failed: Place not found".into(),
                )),
            }
        }
        "enforced" => {
            // Replies and DMs are exempt: same bar as `unenforced`.
            if visibility_is_direct || is_reply {
                return check_place_policy(
                    "unenforced",
                    visibility_is_direct,
                    is_reply,
                    place_id,
                    place_resolves,
                );
            }
            match place_id {
                None => Err(Error::InvalidInput(
                    "Validation failed: Place can't be blank".into(),
                )),
                Some(id) if place_resolves => Ok(Some(id)),
                Some(_) => Err(Error::InvalidInput(
                    "Validation failed: Place not found".into(),
                )),
            }
        }
        unknown => Err(Error::Internal(format!(
            "unknown place_policy configuration: {unknown}"
        ))),
    }
}
