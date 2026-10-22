//! Core extension CORE-T: local-writes-only alt-text policy.
//!
//! Pure policy helpers for `Config.alt_text_policy` (`"off"` | `"nag"` |
//! `"block"`, default `"off"`). See `docs/configuration.md` (core-ext
//! section): the policy applies to **local writes only** (media v1/v2 POST,
//! status create/edit). Federation ingest never drops for missing alt-text —
//! there is deliberately no hook in `application::federation_inbound`.
//!
//! First-class application module (wired in `application/mod.rs`); errors
//! are the shared domain errors so endpoints map them directly.
//! Chosen nag signal: response header
//! [`NAG_HEADER_NAME`] (`X-Birchtree-Alt-Text-Nag: 1`), emitted on success
//! responses (200/202/206 paths alike) of the v1/v2 media and status
//! create/edit endpoints when `policy == "nag"` and at least one
//! image/video/gifv attachment lacks a description. Rationale: a JSON
//! extension field would break the pinned exact-DTO-key contracts (Phase 4
//! media/status tests assert serializer shapes); a header is additive,
//! ignorable by Mastodon clients, uniform across 200 (v1 upload/update,
//! status create/edit) and 202 (v2 deferred video/audio) responses, and needs
//! no serializer changes.

/// Response header carrying the `nag` signal. Present with value `"1"` only
/// when the policy is `"nag"` and at least one covered attachment is missing
/// a description; absent under every other policy/outcome (see
/// [`nag_signal`]).
pub const NAG_HEADER_NAME: &str = "X-Birchtree-Alt-Text-Nag";

/// Header value emitted when the nag applies.
pub const NAG_HEADER_VALUE: &str = "1";

/// 422 body used when the `block` policy refuses a write.
///
/// Rails-shaped (`Validation failed: ...`), mirroring the existing media
/// message style (`"Validation failed: Description is too long (maximum is
/// 10000 characters)"` in `application::media`). This message is new surface
/// (upstream Mastodon has no alt-text gate), so the exact string is pinned
/// here and must be pinned in `docs/endpoint-manifest.md` on merge.
pub const BLOCK_MESSAGE: &str = "Validation failed: Description can't be blank";

use crate::domain::{Error, Result};

/// True when an attachment of the given wire `type` (one of
/// `unknown`/`image`/`gifv`/`video`/`audio`, as stored on
/// `MediaAttachment::type`) must carry a description under an active policy.
///
/// Covered: `image`, `video`, `gifv`. Not covered: `audio`, `unknown`, and
/// any other/empty string (fail-open: an unrecognized type never blocks).
pub fn attachment_needs_description(type_str: &str) -> bool {
    matches!(type_str, "image" | "video" | "gifv")
}

/// True when a description counts as missing: empty or whitespace-only.
pub fn description_missing(description: &str) -> bool {
    description.trim().is_empty()
}

/// True when the policy is active (`nag` or `block`) and at least one covered
/// attachment in `attachments` (`(type, description)` pairs) lacks a
/// description.
///
/// Under `"off"` (or any unrecognized policy value, treated as `off`) this is
/// always false. Text-only writes (empty `attachments`) never report missing.
pub fn missing_description<T: AsRef<str>, D: AsRef<str>>(
    policy: &str,
    attachments: &[(T, D)],
) -> bool {
    if !matches!(policy, "nag" | "block") {
        return false;
    }
    attachments.iter().any(|(type_str, description)| {
        attachment_needs_description(type_str.as_ref())
            && description_missing(description.as_ref())
    })
}

/// Enforce the `block` policy: `Err(InvalidInput)` when `policy == "block"`
/// and any covered attachment lacks a description; `Ok(())` otherwise
/// (`"off"`, `"nag"`, unrecognized policies, uncovered-only or fully
/// described attachments, and text-only writes all pass).
pub fn check_block<T: AsRef<str>, D: AsRef<str>>(
    policy: &str,
    attachments: &[(T, D)],
) -> Result<()> {
    if policy == "block" && missing_description(policy, attachments) {
        return Err(Error::InvalidInput(BLOCK_MESSAGE.to_string()));
    }
    Ok(())
}

/// Whether to emit the nag signal ([`NAG_HEADER_NAME`]): true only when
/// `policy == "nag"` and at least one covered attachment lacks a
/// description. Never true under `"off"`/`"block"`/unrecognized policies;
/// `"block"` refuses the write instead of nagging.
pub fn nag_signal<T: AsRef<str>, D: AsRef<str>>(policy: &str, attachments: &[(T, D)]) -> bool {
    policy == "nag" && missing_description(policy, attachments)
}
