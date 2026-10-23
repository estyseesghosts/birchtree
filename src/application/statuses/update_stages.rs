//! Stage helpers for `PUT /api/v1/statuses/:id` (`update::update_status`).
//!
//! Each helper is a verbatim move of one field/policy block from the
//! orchestrator: error strings, payload shapes and check ordering are
//! unchanged. Field moves from the original (`cmd.poll`, `cmd.media_ids`,
//! …) become helper parameters; pure borrows stay borrows, except
//! `resolve_media` which clones the media list (a `Vec<Id>`) so the call
//! site stays two lines.

use super::create::PollCommand;
use super::create_mentions::language_regex;
use super::create_mentions::render_html;
use super::create_validate::ERR_LANGUAGE;
use super::create_validate::ERR_RECORD_INVALID;
use super::create_validate::build_poll_spec;
use crate::application::ports::StatusUpdate;
use crate::application::ports::Store;
use crate::domain::Error;
use crate::domain::Id;
use crate::domain::QuotePolicy;
use crate::domain::Result;
use crate::domain::Status;
use crate::domain::Visibility;

/// New source text implies new rendered HTML.
pub(super) fn apply_text(text: &Option<String>, update: &mut StatusUpdate) {
    if let Some(text) = text {
        update.source_text = Some(text.clone());
        update.content_html = Some(render_html(text));
    }
}

/// Present-and-blank clears the CW (`Some(None)`, as in Mastodon).
pub(super) fn apply_spoiler(spoiler_text: Option<String>, update: &mut StatusUpdate) {
    if let Some(spoiler) = spoiler_text {
        update.spoiler_text = Some(if spoiler.trim().is_empty() {
            None
        } else {
            Some(spoiler)
        });
    }
}

/// Unknown languages are a 422 (deliberate Mastodon-shaped asymmetry with
/// creation, which drops them).
pub(super) fn apply_language(language: Option<String>, update: &mut StatusUpdate) -> Result<()> {
    if let Some(language) = language {
        if !language_regex().is_match(&language) {
            return Err(Error::InvalidInput(ERR_LANGUAGE.into()));
        }
        update.language = Some(language);
    }
    Ok(())
}

/// Core extension CORE-P: same place gate as creation (`None` leaves the
/// stored place unchanged; independent of media edits). Replies and DMs
/// exempt. Returns the full `update.place_id` field value.
pub(super) fn resolve_place(
    store: &dyn Store,
    place_policy: &str,
    status: &Status,
    place_id: Option<Option<Id>>,
) -> Result<Option<Option<Id>>> {
    let Some(raw_place) = place_id else {
        return Ok(None);
    };
    let resolves = match raw_place {
        None => false,
        Some(id) => store.place_by_id(id)?.is_some(),
    };
    Ok(Some(crate::application::place_policy::check_place_policy(
        place_policy,
        status.visibility == Visibility::Direct,
        status.in_reply_to_id.is_some(),
        raw_place,
        resolves,
    )?))
}

/// Media-list replacement with the CORE-L enforced gate.
///
/// Core extension CORE-L: same enforced gate as creation, evaluated against
/// the resulting attachment set. Replies and DMs exempt. Returns the full
/// `update.media_ids` field value (`None` leaves attachments unchanged).
pub(super) fn resolve_media(
    store: &dyn Store,
    license_policy: &str,
    status: &Status,
    cmd_account_id: Id,
    cmd_media_ids: Option<Vec<Id>>,
) -> Result<Option<Vec<Id>>> {
    if let Some(media_ids) = cmd_media_ids {
        if license_policy == "enforced"
            && status.in_reply_to_id.is_none()
            && status.visibility != Visibility::Direct
            && !media_ids.is_empty()
        {
            let attached = store.media_by_ids(&media_ids)?;
            let author = store
                .account_by_id(cmd_account_id)?
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
        Ok(Some(media_ids))
    } else {
        Ok(None)
    }
}

/// A supplied poll is validated exactly as on creation.
pub(super) fn apply_poll(
    poll: Option<PollCommand>,
    now_ms: u64,
    update: &mut StatusUpdate,
) -> Result<()> {
    if let Some(poll) = poll {
        update.poll = Some(build_poll_spec(now_ms, poll)?);
    }
    Ok(())
}

/// A supplied quote-approval wire value is a 422 when unknown.
pub(super) fn apply_quote_policy(
    quote_approval_policy: Option<String>,
    update: &mut StatusUpdate,
) -> Result<()> {
    if let Some(raw) = quote_approval_policy {
        update.quote_approval_policy = Some(
            QuotePolicy::from_wire(&raw)
                .ok_or_else(|| Error::InvalidInput(ERR_RECORD_INVALID.into()))?,
        );
    }
    Ok(())
}

/// Core extension CORE-T: same block gate as creation, evaluated against the
/// resulting attachment set (patched or stored). Replies are NOT exempt here
/// (the rule covers every write leaving an undescribed image/video);
/// text-only edits pass.
pub(super) fn check_alt_text(
    store: &dyn Store,
    alt_text_policy: &str,
    status: &Status,
    update: &StatusUpdate,
) -> Result<()> {
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
    Ok(())
}
