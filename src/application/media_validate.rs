//! Pure media upload validators (decomposition slice A).
//!
//! Behaviour-verbatim extracts of [`crate::application::media`] upload steps
//! 1-6 (`stage_and_finish_upload`): each function below mirrors one step's
//! rule, error string, and order. The composing caller keeps the staging,
//! store, and job logic and calls these in the same order.

use super::media::{ERR_BLANK_FILE, ERR_CONTENT_TYPE, file_too_large_error};
use super::media::{ERR_DESCRIPTION_TOO_LONG, ERR_THUMBNAIL_BLANK, ERR_THUMBNAIL_INVALID};
use crate::application::alt_text_policy;
use crate::domain::{Error, FocusPoint, Result, license};
use crate::media::ingest::{self, Sniffed};

/// Mastodon's `MediaAttachment::MAX_DESCRIPTION_LENGTH`.
const MAX_DESCRIPTION_LENGTH: usize = 10_000;

/// Rejects a missing/blank file param (upload step 1).
///
/// Mirrors `stage_and_finish_upload` step 1: a zero-length upload is
/// `ERR_BLANK_FILE`.
pub fn check_blank(len: u64) -> Result<()> {
    if len == 0 {
        return Err(Error::InvalidInput(ERR_BLANK_FILE.into()));
    }
    Ok(())
}

/// Sniffs a bounded content prefix, keeping only accepted types (step 2).
///
/// Mirrors `sniff_accepted` over the upload prefix: anything the sniffer
/// rejects (or that falls outside Mastodon's accepted lists) is the pinned
/// content-type 422. The declared multipart type is never trusted.
pub fn sniff_accepted_or_err(prefix: &[u8]) -> Result<Sniffed> {
    ingest::sniff(prefix)
        .filter(|sniffed| ingest::validate(*sniffed))
        .ok_or_else(|| Error::InvalidInput(ERR_CONTENT_TYPE.into()))
}

/// Enforces the configured size cap (upload step 3).
///
/// Mirrors `stage_and_finish_upload` step 3: `len` over `max` answers
/// Mastodon's pinned `File must be less than <human>` 422.
pub fn check_size(len: u64, max: u64) -> Result<()> {
    if len > max {
        return Err(file_too_large_error(max));
    }
    Ok(())
}

/// Enforces Mastodon's description length cap (upload step 4).
///
/// Mirrors `stage_and_finish_upload` step 4: more than
/// `MAX_DESCRIPTION_LENGTH` chars is `ERR_DESCRIPTION_TOO_LONG`.
pub fn check_description(description: &str) -> Result<()> {
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err(Error::InvalidInput(ERR_DESCRIPTION_TOO_LONG.into()));
    }
    Ok(())
}

/// Parses Mastodon's `focus` param (`"x,y"`, both floats) (upload step 5).
///
/// Delegates to [`crate::application::media::parse_focus`]: blank is
/// ignored (`None`); a non-numeric pair is `ERR_FOCUS_INVALID` instead of
/// silently coercing to `(0.0, 0.0)`.
pub fn parse_focus_or_err(focus: Option<&str>) -> Result<Option<FocusPoint>> {
    crate::application::media::parse_focus(focus.unwrap_or_default())
}

/// Enforces the `block` alt-text policy (upload step 5b).
///
/// Mirrors the `check_block` call in `stage_and_finish_upload`: under
/// `"block"`, an undescribed image/video/gifv upload is a 422; every other
/// policy or fully-described write passes.
pub fn check_alt_text(policy: &str, media_type: &str, description: &str) -> Result<()> {
    alt_text_policy::check_block(policy, &[(media_type, description)])
}

/// Resolves the CORE-L license param (upload step 5c).
///
/// Mirrors `stage_and_finish_upload` step 5c: under `"off"` the param is
/// accepted-and-ignored entirely (not even parsed); otherwise an out-of-enum
/// value is a 422 via `parse_license`.
pub fn resolve_license(policy: &str, raw: Option<i64>) -> Result<Option<u8>> {
    if policy == "off" {
        Ok(None)
    } else {
        license::parse_license(raw)
    }
}

/// Validates the operator-supplied cover image (upload step 6).
///
/// Mirrors `stage_and_finish_upload` step 6 in order: absent/empty yields
/// `None`; a thumbnail on a non-video/audio upload is `ERR_THUMBNAIL_BLANK`;
/// an over-cap thumbnail answers the pinned size 422; a non-image thumbnail
/// is `ERR_THUMBNAIL_INVALID`. Returns the validated `(bytes, sniff)` pair
/// (the caller owns both) for staging.
pub fn check_thumbnail<'t>(
    sniffed: Sniffed,
    thumbnail: Option<&'t [u8]>,
    max_file_size: u64,
) -> Result<Option<(&'t [u8], Sniffed)>> {
    let bytes = match thumbnail {
        None | Some(&[]) => return Ok(None),
        Some(bytes) => bytes,
    };
    if !ingest::is_large_format(sniffed) {
        return Err(Error::InvalidInput(ERR_THUMBNAIL_BLANK.into()));
    }
    if bytes.len() as u64 > max_file_size {
        return Err(file_too_large_error(max_file_size));
    }
    let thumb = ingest::sniff(bytes)
        .filter(|sniffed| ingest::validate(*sniffed))
        .filter(|sniffed| ingest::is_image(*sniffed))
        .ok_or_else(|| Error::InvalidInput(ERR_THUMBNAIL_INVALID.into()))?;
    Ok(Some((bytes, thumb)))
}
