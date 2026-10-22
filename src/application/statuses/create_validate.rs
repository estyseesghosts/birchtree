//! Validation limits and error strings for status composition.
//!
//! Mastodon-compatible limits and verbatim error strings (v4.7.3, pinned).
//! The `ERR_*` strings are byte-identical to the messages asserted by the
//! existing status tests; do not reword them.

use std::collections::HashSet;

use super::create::PollCommand;
use super::create_mentions::url_regex;
use crate::application::ports::NewPollSpec;
use crate::domain::Error;
use crate::domain::Result;

/// Default status text limit (`StatusLengthValidator::MAX_CHARS`); URLs count
/// as a fixed 23 characters (see [`count_length`]). Configurable per server
/// up to [`ABS_MAX_STATUS_LENGTH`] via `--max-status-chars=`.
pub const DEFAULT_MAX_STATUS_LENGTH: usize = 500;
/// Hard ceiling on the configurable status text limit (Misskey parity).
pub const ABS_MAX_STATUS_LENGTH: usize = 3000;
/// Every URL counts as this many characters (`URL_PLACEHOLDER`).
const URL_LENGTH: usize = 23;
/// Mastodon's `Status::MEDIA_ATTACHMENTS_LIMIT` for a single status.
pub(crate) const MAX_MEDIA_ATTACHMENTS: usize = 4;
/// Poll options: `PollOptionsValidator` bounds.
const MIN_POLL_OPTIONS: usize = 2;
const MAX_POLL_OPTIONS: usize = 4;
const MAX_POLL_OPTION_CHARS: usize = 50;
/// Poll expiry: `PollExpirationValidator` bounds (5 minutes .. 1.month).
const MIN_POLL_EXPIRY_S: u64 = 300;
const MAX_POLL_EXPIRY_S: u64 = 2_592_000;

/// Rejected when a status carries more than [`MAX_MEDIA_ATTACHMENTS`].
pub(crate) const ERR_TOO_MANY_MEDIA: &str = "Cannot attach more than 4 files";
/// Rejected when no text, media, poll, or quote is present.
pub(crate) const ERR_BLANK_TEXT: &str = "Validation failed: Text can't be blank";
/// Prefix of the over-long-text error; the configured limit is interpolated
/// (Mastodon embeds the number: "character limit of 500 exceeded").
const ERR_TEXT_TOO_LONG_PREFIX: &str = "Validation failed: character limit of ";
/// Rejected when the wire visibility value is unknown.
pub(crate) const ERR_VISIBILITY: &str =
    "Validation failed: Visibility must be public, unlisted, private or direct";
/// Rejected when an edit supplies an unknown language tag.
pub(crate) const ERR_LANGUAGE: &str = "Validation failed: Language is invalid";
/// Rejected when a supplied approval-policy wire value is unknown.
pub(crate) const ERR_RECORD_INVALID: &str = "Record invalid";
/// Rejected when a direct post quotes a non-mentioned user.
pub(crate) const ERR_QUOTE_NOT_MENTIONED: &str =
    "Validation failed: Cannot quote a non-mentioned user in a Private Mention post.";
/// Rejected when a poll has fewer than two options.
pub(crate) const ERR_POLL_TOO_FEW: &str = "Validation failed: Choices must have more than one item";
/// Rejected when a poll has more than four options.
pub(crate) const ERR_POLL_TOO_MANY: &str =
    "Validation failed: Choices can't contain more than 4 items";
/// Rejected when a poll option exceeds 50 characters.
pub(crate) const ERR_POLL_OPTION_TOO_LONG: &str =
    "Validation failed: Choices cannot be longer than 50 characters each";
/// Rejected when poll options contain case-insensitive duplicates.
pub(crate) const ERR_POLL_DUPLICATE_OPTIONS: &str =
    "Validation failed: Choices contain duplicate items";
/// Rejected when a poll expires sooner than 5 minutes.
pub(crate) const ERR_POLL_EXPIRY_TOO_SHORT: &str = "Validation failed: Deadline is too soon";
/// Rejected when a poll expires later than 30 days.
pub(crate) const ERR_POLL_EXPIRY_TOO_LONG: &str =
    "Validation failed: Deadline is too far into the future";

/// 404 body for an unfetchable/unknown reply parent (Mastodon
/// `statuses.errors.in_reply_not_found`).
pub(crate) const ERR_REPLY_NOT_FOUND: &str =
    "The post you are trying to reply to does not appear to exist.";
/// 404 body for an unquotable/unknown quoted status
/// (`statuses.errors.quoted_status_not_found`).
pub(crate) const ERR_QUOTE_NOT_FOUND: &str =
    "The post you are trying to quote does not appear to exist.";

/// Over-long-text error with the configured limit interpolated, matching
/// Mastodon's message shape (`character limit of 500 exceeded` at default).
pub fn text_too_long_error(max_chars: usize) -> String {
    format!("{ERR_TEXT_TOO_LONG_PREFIX}{max_chars} exceeded")
}

/// Count the status length the way Mastodon does: every URL matches
/// `https?://\S+` and counts as 23 characters, every other character as 1.
pub fn count_length(text: &str) -> usize {
    let re = url_regex();
    let mut total = 0usize;
    let mut last = 0usize;
    for m in re.find_iter(text) {
        total += text[last..m.start()].chars().count();
        total += URL_LENGTH;
        last = m.end();
    }
    total + text[last..].chars().count()
}

/// Validate a poll command and turn it into the store's `NewPollSpec`
/// (`expires_at_ms` is absolute: `now + expires_in_s`).
///
/// Messages mirror `PollOptionsValidator` / `PollExpirationValidator` wrapped
/// in `ActiveRecord::RecordInvalid`'s "Validation failed: " prefix.
pub(crate) fn build_poll_spec(now_ms: u64, cmd: PollCommand) -> Result<NewPollSpec> {
    let options: Vec<String> = cmd
        .options
        .iter()
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty())
        .collect();

    if options.len() < MIN_POLL_OPTIONS {
        return Err(Error::InvalidInput(ERR_POLL_TOO_FEW.into()));
    }
    if options.len() > MAX_POLL_OPTIONS {
        return Err(Error::InvalidInput(ERR_POLL_TOO_MANY.into()));
    }
    if options
        .iter()
        .any(|o| o.chars().count() > MAX_POLL_OPTION_CHARS)
    {
        return Err(Error::InvalidInput(ERR_POLL_OPTION_TOO_LONG.into()));
    }
    let mut seen = HashSet::new();
    if options.iter().any(|o| !seen.insert(o.to_lowercase())) {
        return Err(Error::InvalidInput(ERR_POLL_DUPLICATE_OPTIONS.into()));
    }
    if cmd.expires_in_s < MIN_POLL_EXPIRY_S {
        return Err(Error::InvalidInput(ERR_POLL_EXPIRY_TOO_SHORT.into()));
    }
    if cmd.expires_in_s > MAX_POLL_EXPIRY_S {
        return Err(Error::InvalidInput(ERR_POLL_EXPIRY_TOO_LONG.into()));
    }

    Ok(NewPollSpec {
        multiple: cmd.multiple,
        hide_totals: cmd.hide_totals,
        expires_at_ms: now_ms + cmd.expires_in_s * 1000,
        options,
    })
}
