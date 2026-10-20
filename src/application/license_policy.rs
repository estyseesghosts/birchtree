//! Core extension CORE-L: license policy gating (Stage 1).
//!
//! Pure functions only (no DB): the effective license of one attachment,
//! the enforced-policy gate for status create/edit, and the wire mapping
//! the DTO layer renders. All Mastodon-parity behavior is preserved when
//! the policy is `off` (the default).
//!
//! Rules (per `docs/configuration.md`):
//! - `off`: license params are accepted-and-ignored (stored `NULL`); the
//!   wire omits `license`/`preferredLicense`; no gate ever fires here.
//! - `unenforced`: licenses round-trip (local + wire); out-of-enum values
//!   422 at [`crate::domain::license::parse_license`]; no gate fires here.
//! - `enforced`: as `unenforced`, plus the effective license
//!   (per-attachment else author default) must be non-`NULL` on top-level
//!   non-DM statuses carrying media, else a Mastodon-compatible 422.
//!   Replies and DMs are exempt. Media upload alone never 422s.

use crate::domain::{Error, Result};

/// 422 body when the enforced gate finds unlicensed media on a gated
/// status (Mastodon `RecordInvalid` shape).
pub const ERR_LICENSE_REQUIRED: &str =
    "Validation failed: License is required for media attachments";

/// Effective license of one attachment: the per-attachment license wins,
/// otherwise the author's default applies. `None` = all rights reserved.
pub fn effective_license(
    attachment_license: Option<u8>,
    author_default: Option<u8>,
) -> Option<u8> {
    attachment_license.or(author_default)
}

/// Gate for status create/edit. `off`/`unenforced` (and any unrecognized
/// policy string) never error here — out-of-enum 422s happen at parse.
/// Under `enforced`, errors ([`Error::InvalidInput`]) when the status is a
/// top-level (`!is_reply`) non-DM (`!visibility_is_direct`) post carrying
/// media (`has_media`) and any effective license is `None`.
pub fn check_status_licenses(
    policy: &str,
    visibility_is_direct: bool,
    is_reply: bool,
    has_media: bool,
    effective_licenses: &[Option<u8>],
) -> Result<()> {
    if policy != "enforced" {
        return Ok(());
    }
    if visibility_is_direct || is_reply || !has_media {
        return Ok(());
    }
    if effective_licenses.iter().any(|license| license.is_none()) {
        return Err(Error::InvalidInput(ERR_LICENSE_REQUIRED.into()));
    }
    Ok(())
}

/// Wire mapping for one `license` value under `policy`: `None` means the
/// caller omits the key (disabled policy); otherwise the value renders
/// (`Some(number)` for a license, `Some(null)` for all-rights).
pub fn license_wire_value(policy: &str, license: Option<u8>) -> Option<serde_json::Value> {
    if policy == "off" {
        return None;
    }
    Some(match license {
        Some(id) => serde_json::Value::from(id),
        None => serde_json::Value::Null,
    })
}
