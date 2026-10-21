//! Aperture-mode write gate (Stage 2 AP-A1).
//!
//! Pure function over `Config.aperture_mode` (bool, default `false`). No DB
//! access: the caller already knows the write's visibility (`direct` or not),
//! whether it is a reply, and whether it carries media, so this gate takes
//! those as plain bools.
//!
//! Message choice: there is no upstream Mastodon string for this gate
//! (aperture mode is a birchtree Stage 2 extension, not upstream behavior).
//! The message below keeps the Rails `ActiveRecord::RecordInvalid`
//! `"Validation failed: "` prefix used by every other status-write 422 in
//! `application::statuses` (e.g. `ERR_BLANK_TEXT`), with a body in the style
//! of the CORE-P blank error (`"Place can't be blank"`).
//!
//! Proposed wiring (not applied): `CreateStatusCommand` /
//! `UpdateStatusCommand` gain `aperture_mode: bool`; `create_status` /
//! `update_status` in `src/application/statuses.rs` call
//! [`check_aperture_write`] after reply resolution (top-level vs reply is
//! known there), passing `!media_ids.is_empty()` as `has_media`.
//!
//! Wire-up is proposed (not applied): add `pub mod aperture;` to
//! `src/application/mod.rs`.

use crate::domain::{Error, Result};

/// 422 body when aperture mode rejects a media-less top-level non-DM write.
///
/// No upstream equivalent exists (see module docs); the `Validation failed: `
/// prefix keeps the Rails-shaped 422 family used by all sibling gates.
pub const ERR_APERTURE_MEDIA_REQUIRED: &str =
    "Validation failed: Status must have media attachments";

/// Apply the aperture write gate to one status write.
///
/// - `aperture`: `Config.aperture_mode`. When `false` (default, Mastodon
///   parity) every write passes — the gate is inert.
/// - `visibility_is_direct`: true for direct-visibility (DM) statuses, which
///   are always exempt.
/// - `is_reply`: true when the status is a reply (`in_reply_to_id.is_some()`),
///   which is always exempt.
/// - `has_media`: true when the write carries at least one attachment
///   (`!media_ids.is_empty()` on create; the resulting attachment set on
///   edit).
///
/// Returns `Ok(())` except when `aperture && !direct && !is_reply &&
/// !has_media`, which is `Err(InvalidInput(ERR_APERTURE_MEDIA_REQUIRED))`.
/// No other behavior: in particular the gate never inspects policy strings,
/// never touches the store, and forces nothing about the three Stage 1 core
/// extension policies (license/place/alt-text).
pub fn check_aperture_write(
    aperture: bool,
    visibility_is_direct: bool,
    is_reply: bool,
    has_media: bool,
) -> Result<()> {
    if aperture && !visibility_is_direct && !is_reply && !has_media {
        return Err(Error::InvalidInput(ERR_APERTURE_MEDIA_REQUIRED.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_passes_everything() {
        // Gate inert at default: even a text-only top-level passes.
        assert!(check_aperture_write(false, false, false, false).is_ok());
        assert!(check_aperture_write(false, true, true, true).is_ok());
    }

    #[test]
    fn on_blocks_only_text_only_top_level_non_dm() {
        assert!(check_aperture_write(true, false, false, false).is_err());
        assert!(check_aperture_write(true, false, false, true).is_ok());
        // Replies and DMs exempt regardless of media.
        assert!(check_aperture_write(true, false, true, false).is_ok());
        assert!(check_aperture_write(true, true, false, false).is_ok());
        assert!(check_aperture_write(true, true, true, false).is_ok());
    }

    #[test]
    fn error_is_invalid_input_with_prefix() {
        let err = check_aperture_write(true, false, false, false).expect_err("must reject");
        match err {
            Error::InvalidInput(msg) => {
                assert_eq!(msg, ERR_APERTURE_MEDIA_REQUIRED);
                assert!(msg.starts_with("Validation failed: "), "{msg}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }
}
