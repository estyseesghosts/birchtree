//! Status creation orchestrator (`POST /api/v1/statuses`).
//!
//! Validation order follows Mastodon's `PostStatusService`: defaults and the
//! content-warning folding rule, media/visibility/language sanity, text
//! length/blank checks, reply resolution, mention/tag extraction, quote
//! resolution (visibility is finalized before the direct-mention rule), then
//! the poll.

use super::create_mentions::canonical_tags;
use super::create_mentions::extract_mention_handles;
use super::create_mentions::language_regex;
use super::create_mentions::render_html;
use super::create_mentions::resolve_mentions;
use super::create_validate::ERR_BLANK_TEXT;
use super::create_validate::ERR_QUOTE_NOT_FOUND;
use super::create_validate::ERR_QUOTE_NOT_MENTIONED;
use super::create_validate::ERR_RECORD_INVALID;
use super::create_validate::ERR_REPLY_NOT_FOUND;
use super::create_validate::ERR_TOO_MANY_MEDIA;
use super::create_validate::ERR_VISIBILITY;
use super::create_validate::MAX_MEDIA_ATTACHMENTS;
use super::create_validate::build_poll_spec;
use super::create_validate::count_length;
use super::create_validate::text_too_long_error;
use crate::application::accounts::host_from_url;
use crate::application::policy;
use crate::application::ports::NewQuoteSpec;
use crate::application::ports::NewStatus;
use crate::application::ports::Store;
use crate::domain::Error;
use crate::domain::Id;
use crate::domain::InteractionEvent;
use crate::domain::InteractionEventKind;
use crate::domain::QuotePolicy;
use crate::domain::Result;
use crate::domain::Status;
use crate::domain::StatusApplication;
use crate::domain::Visibility;

/// A poll to embed in a status being created or edited.
#[derive(Debug, Clone)]
pub struct PollCommand {
    pub multiple: bool,
    pub hide_totals: bool,
    /// Expiry interval in seconds from now (5 minutes .. 30 days).
    pub expires_in_s: u64,
    pub options: Vec<String>,
}

/// Input for `POST /api/v1/statuses`.
///
/// `visibility` and `quote_approval_policy` are wire values validated by
/// [`create_status`]; `mentions`/tags are extracted from `text` here.
#[derive(Debug, Clone)]
pub struct CreateStatusCommand {
    pub account_id: Id,
    pub text: Option<String>,
    pub in_reply_to_id: Option<Id>,
    pub quoted_status_id: Option<Id>,
    /// Wire value, validated here.
    pub quote_approval_policy: Option<String>,
    pub sensitive: Option<bool>,
    pub spoiler_text: Option<String>,
    /// Wire value, validated here.
    pub visibility: Option<String>,
    pub language: Option<String>,
    pub media_ids: Vec<Id>,
    pub poll: Option<PollCommand>,
    pub application_name: Option<String>,
    pub application_website: Option<String>,
    pub idempotency_key: Option<String>,
}

/// Result of a status creation; `replayed` is true when an idempotent retry
/// returned the originally created status.
#[derive(Debug, Clone)]
pub struct CreateStatusResult {
    pub status: Status,
    pub replayed: bool,
}

/// `POST /api/v1/statuses` — validate and create a status.
///
/// Validation order follows Mastodon's `PostStatusService`: defaults and the
/// content-warning folding rule, media/visibility/language sanity, text
/// length/blank checks, reply resolution, mention/tag extraction, quote
/// resolution (visibility is finalized before the direct-mention rule), then
/// the poll. The store commits the record, poll/quote, indexes, counters and
/// idempotency record in ONE transaction; a replayed `Idempotency-Key`
/// returns the original status with `replayed = true`.
pub fn create_status(
    store: &dyn Store,
    base_url: &str,
    now_ms: u64,
    cmd: CreateStatusCommand,
    max_chars: usize,
    license_policy: &str,
    place_policy: &str,
    place_id: Option<Id>,
    alt_text_policy: &str,
) -> Result<CreateStatusResult> {
    let CreateStatusCommand {
        account_id,
        text,
        in_reply_to_id,
        quoted_status_id,
        quote_approval_policy,
        sensitive,
        spoiler_text,
        visibility,
        language,
        media_ids,
        poll,
        application_name,
        application_website,
        idempotency_key,
    } = cmd;

    let account = store
        .account_by_id(account_id)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
    let user = store
        .user_by_account_id(account_id)?
        .ok_or_else(|| Error::NotFound("account".into()))?;

    // Text and content warning. A blank body with only a CW (and nothing
    // else to show) folds the CW into the body, as Mastodon does.
    let mut text = text.unwrap_or_default();
    let spoiler = spoiler_text.and_then(|s| if s.trim().is_empty() { None } else { Some(s) });
    let cw_only = text.trim().is_empty()
        && spoiler.is_some()
        && quoted_status_id.is_none()
        && poll.is_none()
        && media_ids.is_empty();
    if cw_only {
        if let Some(body) = &spoiler {
            text = body.clone();
        }
    }

    if media_ids.len() > MAX_MEDIA_ATTACHMENTS {
        return Err(Error::InvalidInput(ERR_TOO_MANY_MEDIA.into()));
    }

    // Visibility: explicit param > user default > locked accounts default to
    // followers-only. Unknown wire values are a validation error.
    let raw_visibility = visibility
        .or(user.default_privacy.clone())
        .unwrap_or_else(|| {
            if account.locked {
                "private".to_string()
            } else {
                "public".to_string()
            }
        });
    let mut visibility = Visibility::from_wire(&raw_visibility)
        .ok_or_else(|| Error::InvalidInput(ERR_VISIBILITY.into()))?;

    let mut sensitive = sensitive.or(user.default_sensitive).unwrap_or(false);
    if spoiler.is_some() {
        sensitive = true;
    }

    // Unknown language tags are dropped rather than failing the post.
    let language = language
        .or(user.default_language.clone())
        .filter(|l| language_regex().is_match(l));

    if count_length(&text) > max_chars {
        return Err(Error::InvalidInput(text_too_long_error(max_chars)));
    }
    if text.trim().is_empty()
        && media_ids.is_empty()
        && poll.is_none()
        && quoted_status_id.is_none()
    {
        return Err(Error::InvalidInput(ERR_BLANK_TEXT.into()));
    }

    // Reply parent: missing, deleted or not visible to the author all answer
    // the same 404 (Mastodon `set_thread`).
    let mut in_reply_to = None;
    let mut in_reply_to_account = None;
    if let Some(parent_id) = in_reply_to_id {
        let parent = store
            .status_by_id(parent_id)?
            .ok_or_else(|| Error::NotFound(ERR_REPLY_NOT_FOUND.into()))?;
        let pair = store.pair_state(parent.account_id, account_id, now_ms)?;
        if !policy::can_fetch(Some(account_id), &parent, pair) {
            return Err(Error::NotFound(ERR_REPLY_NOT_FOUND.into()));
        }
        in_reply_to = Some(parent.id);
        in_reply_to_account = Some(parent.account_id);
    }

    // Pre-resolved mentions and tags. The quoted author's silent mention is
    // added by the store via `NewQuoteSpec.quoted_account_id`, not here.
    let handles = extract_mention_handles(&text);
    // D25: normalize at write — `#café` and `#cafe` store as one tag.
    let tags = canonical_tags(&text);
    let local_domain = host_from_url(base_url);
    let mentions = resolve_mentions(store, &handles, Some(&local_domain));

    // Quote approval policy: validated whenever supplied (or needed), from
    // the param, then the user default, then `public`.
    let mut quote_approval = None;
    if quote_approval_policy.is_some() || quoted_status_id.is_some() {
        let raw = quote_approval_policy
            .as_deref()
            .or(user.default_quote_policy.as_deref())
            .unwrap_or("public");
        quote_approval = Some(
            QuotePolicy::from_wire(raw)
                .ok_or_else(|| Error::InvalidInput(ERR_RECORD_INVALID.into()))?,
        );
    }

    // Quote resolution. Visibility is finalized (a quoted followers-only/DM
    // post forces this post to followers-only) BEFORE the direct-mention
    // rule, and the approval policy is downgraded to `nobody` for
    // followers-only/direct posts.
    let mut quote = None;
    if let Some(quoted_id) = quoted_status_id {
        let mut quoted = store
            .status_by_id(quoted_id)?
            .ok_or_else(|| Error::NotFound(ERR_QUOTE_NOT_FOUND.into()))?;
        if let Some(proper_id) = quoted.reblog_of_id {
            quoted = store
                .status_by_id(proper_id)?
                .ok_or_else(|| Error::NotFound(ERR_QUOTE_NOT_FOUND.into()))?;
        }
        let pair = store.pair_state(quoted.account_id, account_id, now_ms)?;
        if policy::quote_permission(Some(account_id), &quoted, pair)
            == policy::QuotePermission::Denied
        {
            return Err(Error::NotFound(ERR_QUOTE_NOT_FOUND.into()));
        }

        if matches!(quoted.visibility, Visibility::Private | Visibility::Direct)
            && matches!(visibility, Visibility::Public | Visibility::Unlisted)
        {
            visibility = Visibility::Private;
        }

        if visibility == Visibility::Direct
            && quoted.account_id != account_id
            && !mentions.contains(&quoted.account_id)
        {
            return Err(Error::InvalidInput(ERR_QUOTE_NOT_MENTIONED.into()));
        }

        let mut approval = quote_approval.unwrap_or(QuotePolicy::Public);
        if matches!(visibility, Visibility::Private | Visibility::Direct) {
            approval = QuotePolicy::Nobody;
        }
        quote = Some(NewQuoteSpec {
            quoted_status_id: quoted.id,
            quoted_account_id: quoted.account_id,
            approval_policy: approval,
        });
    }

    let poll = poll.map(|p| build_poll_spec(now_ms, p)).transpose()?;

    // Core extension CORE-P: the place gate mirrors the license gate above.
    // Under `off` the param is accepted-and-ignored (stored NULL); otherwise
    // an unknown id takes the existing 422 path, and `enforced` additionally
    // requires a resolvable place on top-level non-DM statuses. Replies and
    // DMs are exempt.
    let place_resolves = match place_id {
        None => false,
        Some(id) => store.place_by_id(id)?.is_some(),
    };
    let place_id = crate::application::place_policy::check_place_policy(
        place_policy,
        visibility == Visibility::Direct,
        in_reply_to.is_some(),
        place_id,
        place_resolves,
    )?;

    // Core extension CORE-L: under `enforced`, a top-level non-DM status
    // carrying media needs an effective license (per-attachment else author
    // default) on every attachment. Replies and DMs are exempt; the media
    // upload itself never gated. Missing rows are left to the store's
    // attachability check below (same 422 family).
    if license_policy == "enforced"
        && visibility != Visibility::Direct
        && in_reply_to.is_none()
        && !media_ids.is_empty()
    {
        let attached = store.media_by_ids(&media_ids)?;
        let mut effective = Vec::with_capacity(media_ids.len());
        for id in &media_ids {
            if let Some(media) = attached.get(id) {
                effective.push(crate::application::license_policy::effective_license(
                    media.license,
                    account.default_license,
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

    // Core extension CORE-T: under `block`, a top-level or reply write
    // leaving an image/video/gifv attachment without a description is a
    // 422 (local writes only; text-only posts unaffected). Missing rows
    // are left to the store's attachability check (same 422 family).
    if matches!(alt_text_policy, "nag" | "block") && !media_ids.is_empty() {
        let attached = store.media_by_ids(&media_ids)?;
        let mut pairs = Vec::with_capacity(media_ids.len());
        for id in &media_ids {
            if let Some(media) = attached.get(id) {
                pairs.push((media.r#type.clone(), media.description.clone()));
            }
        }
        if pairs.len() == media_ids.len() {
            crate::application::alt_text_policy::check_block(alt_text_policy, &pairs)?;
        }
    }

    let base = base_url.trim_end_matches('/');
    let content_html = render_html(&text);
    let created = store.create_status(
        NewStatus {
            remote: None,
            account_id,
            uri_base: format!("{base}/users/{}/statuses", account.username),
            url_base: format!("{base}/@{}", account.username),
            source_text: text,
            content_html,
            spoiler_text: spoiler,
            sensitive,
            visibility,
            language,
            in_reply_to_id: in_reply_to,
            in_reply_to_account_id: in_reply_to_account,
            mentions,
            // The quoted author's silent mention is appended by the store
            // from `NewQuoteSpec.quoted_account_id`, never via `mentions`.
            silent_mentions: Vec::new(),
            tags,
            media_ids,
            place_id,
            poll,
            quote,
            application: application_name.map(|name| StatusApplication {
                name,
                website: application_website,
            }),
            idempotency_key,
        },
        now_ms,
    )?;

    // D39 `interactions` journal: upstream counts ONLY non-self replies
    // (`PostStatusService#bump_potential_friendship!`,
    // `app/services/post_status_service.rb:262-266`). Idempotent replays
    // (`replayed`) and top-level/self-reply posts journal nothing. The store
    // mints the event id; a journal failure never fails the post itself
    // (metrics must not break the write path — same fire-and-forget shape as
    // `record_sign_in` at `api/oauth/authorization.rs:272`).
    if !created.replayed && in_reply_to.is_some() && in_reply_to_account != Some(account_id) {
        let _ = store.record_interaction(InteractionEvent::for_journal(
            account_id,
            InteractionEventKind::Reply,
            now_ms,
        ));
    }

    Ok(CreateStatusResult {
        status: created.status,
        replayed: created.replayed,
    })
}
