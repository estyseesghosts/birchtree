//! Stage helpers for `POST /api/v1/statuses` (`create::create_status`).
//!
//! Each helper is a verbatim move of one validation/policy block from the
//! orchestrator: error strings, payload shapes and check ordering are
//! unchanged. The orchestrator keeps its exact 9-parameter signature and
//! stays under 80 lines by calling these stages in the same order.
//!
//! Calling convention: stages that need request fields take the whole
//! `CreateStatusCommand` by reference (field accesses, one clone where the
//! original moved) so call sites stay one line; the final assembly takes a
//! `StatusParts` struct plus the leftover owned command fields.

use super::create::{CreateStatusCommand, PollCommand};
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
use crate::application::ports::NewPollSpec;
use crate::application::ports::NewQuoteSpec;
use crate::application::ports::NewStatus;
use crate::application::ports::Store;
use crate::domain::Account;
use crate::domain::Error;
use crate::domain::Id;
use crate::domain::InteractionEvent;
use crate::domain::InteractionEventKind;
use crate::domain::LocalUser;
use crate::domain::QuotePolicy;
use crate::domain::Result;
use crate::domain::StatusApplication;
use crate::domain::Visibility;

/// Assembled inputs for [`build_new_status`]: computed stages plus the owned
/// command tail the orchestrator has not consumed yet.
pub(super) struct StatusParts {
    pub(super) account_id: Id,
    pub(super) text: String,
    pub(super) spoiler: Option<String>,
    pub(super) sensitive: bool,
    pub(super) visibility: Visibility,
    pub(super) language: Option<String>,
    pub(super) in_reply_to: Option<Id>,
    pub(super) in_reply_to_account: Option<Id>,
    pub(super) mentions: Vec<Id>,
    pub(super) tags: Vec<String>,
    pub(super) media_ids: Vec<Id>,
    pub(super) place_id: Option<Id>,
    pub(super) poll: Option<NewPollSpec>,
    pub(super) quote: Option<NewQuoteSpec>,
    pub(super) application_name: Option<String>,
    pub(super) application_website: Option<String>,
    pub(super) idempotency_key: Option<String>,
}

/// Load the author and its local preferences (404 when either row is missing).
pub(super) fn load_author(store: &dyn Store, account_id: Id) -> Result<(Account, LocalUser)> {
    let account = store
        .account_by_id(account_id)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
    let user = store
        .user_by_account_id(account_id)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
    Ok((account, user))
}

/// Text and content warning. A blank body with only a CW (and nothing else
/// to show) folds the CW into the body, as Mastodon does.
pub(super) fn fold_text(cmd: &CreateStatusCommand) -> (String, Option<String>) {
    let mut text = cmd.text.clone().unwrap_or_default();
    let spoiler = cmd
        .spoiler_text
        .clone()
        .and_then(|s| if s.trim().is_empty() { None } else { Some(s) });
    let cw_only = text.trim().is_empty()
        && spoiler.is_some()
        && cmd.quoted_status_id.is_none()
        && cmd.poll.is_none()
        && cmd.media_ids.is_empty();
    if cw_only {
        if let Some(body) = &spoiler {
            text = body.clone();
        }
    }
    (text, spoiler)
}

/// Reject a status carrying more than the attachment cap.
pub(super) fn check_media_count(media_ids: &[Id]) -> Result<()> {
    if media_ids.len() > MAX_MEDIA_ATTACHMENTS {
        return Err(Error::InvalidInput(ERR_TOO_MANY_MEDIA.into()));
    }
    Ok(())
}

/// Visibility, sensitivity and language defaults.
///
/// Visibility: explicit param > user default > locked accounts default to
/// followers-only. Unknown wire values are a validation error.
pub(super) fn resolve_defaults(
    user: &LocalUser,
    account: &Account,
    cmd: &CreateStatusCommand,
    spoiler: &Option<String>,
) -> Result<(Visibility, bool, Option<String>)> {
    let raw_visibility = cmd
        .visibility
        .clone()
        .or(user.default_privacy.clone())
        .unwrap_or_else(|| {
            if account.locked {
                "private".to_string()
            } else {
                "public".to_string()
            }
        });
    // NOTE: `mut` dropped from the monolith — the followers-only downgrade
    // now happens in `resolve_quote` via the returned value.
    let visibility = Visibility::from_wire(&raw_visibility)
        .ok_or_else(|| Error::InvalidInput(ERR_VISIBILITY.into()))?;

    let mut sensitive = cmd.sensitive.or(user.default_sensitive).unwrap_or(false);
    if spoiler.is_some() {
        sensitive = true;
    }

    // Unknown language tags are dropped rather than failing the post.
    let language = cmd
        .language
        .clone()
        .or(user.default_language.clone())
        .filter(|l| language_regex().is_match(l));
    Ok((visibility, sensitive, language))
}

/// Text length and blankness rules (media-count check runs first, as in the
/// monolith: both live here so the ordering cannot drift).
pub(super) fn check_text_shape(
    text: &str,
    cmd: &CreateStatusCommand,
    max_chars: usize,
) -> Result<()> {
    if count_length(text) > max_chars {
        return Err(Error::InvalidInput(text_too_long_error(max_chars)));
    }
    if text.trim().is_empty()
        && cmd.media_ids.is_empty()
        && cmd.poll.is_none()
        && cmd.quoted_status_id.is_none()
    {
        return Err(Error::InvalidInput(ERR_BLANK_TEXT.into()));
    }
    Ok(())
}

/// Reply parent plus pre-resolved mentions and tags.
///
/// Reply parent: missing, deleted or not visible to the author all answer
/// the same 404 (Mastodon `set_thread`). The quoted author's silent mention
/// is added by the store via `NewQuoteSpec.quoted_account_id`, not here.
pub(super) fn resolve_thread(
    store: &dyn Store,
    account_id: Id,
    cmd: &CreateStatusCommand,
    text: &str,
    base_url: &str,
    now_ms: u64,
) -> Result<(Option<Id>, Option<Id>, Vec<Id>, Vec<String>)> {
    let mut in_reply_to = None;
    let mut in_reply_to_account = None;
    if let Some(parent_id) = cmd.in_reply_to_id {
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

    let handles = extract_mention_handles(text);
    // D25: normalize at write — `#café` and `cafe` store as one tag.
    let tags = canonical_tags(text);
    let local_domain = host_from_url(base_url);
    let mentions = resolve_mentions(store, &handles, Some(&local_domain));
    Ok((in_reply_to, in_reply_to_account, mentions, tags))
}

/// Quote approval policy (validated whenever supplied or needed) and quote
/// resolution.
///
/// Visibility is finalized (a quoted followers-only/DM post forces this
/// post to followers-only) BEFORE the direct-mention rule, and the approval
/// policy is downgraded to `nobody` for followers-only/direct posts.
pub(super) fn resolve_quote(
    store: &dyn Store,
    cmd: &CreateStatusCommand,
    user: &LocalUser,
    account_id: Id,
    visibility: Visibility,
    mentions: &[Id],
    now_ms: u64,
) -> Result<(Option<NewQuoteSpec>, Visibility)> {
    // Quote approval policy: validated whenever supplied (or needed), from
    // the param, then the user default, then `public`.
    let mut quote_approval = None;
    if cmd.quote_approval_policy.is_some() || cmd.quoted_status_id.is_some() {
        let raw = cmd
            .quote_approval_policy
            .as_deref()
            .or(user.default_quote_policy.as_deref())
            .unwrap_or("public");
        quote_approval = Some(
            QuotePolicy::from_wire(raw)
                .ok_or_else(|| Error::InvalidInput(ERR_RECORD_INVALID.into()))?,
        );
    }

    let mut quote = None;
    let mut visibility = visibility;
    if let Some(quoted_id) = cmd.quoted_status_id {
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
    Ok((quote, visibility))
}

/// Validate the supplied poll exactly as on creation.
pub(super) fn assemble_poll(poll: Option<PollCommand>, now_ms: u64) -> Result<Option<NewPollSpec>> {
    poll.map(|spec| build_poll_spec(now_ms, spec)).transpose()
}

/// Core extension CORE-P: the place gate. Under `off` the param is
/// accepted-and-ignored (stored NULL); otherwise an unknown id takes the
/// existing 422 path, and `enforced` additionally requires a resolvable
/// place on top-level non-DM statuses. Replies and DMs are exempt.
pub(super) fn check_place_gate(
    store: &dyn Store,
    place_policy: &str,
    visibility: Visibility,
    in_reply_to: Option<Id>,
    place_id: Option<Id>,
) -> Result<Option<Id>> {
    let place_resolves = match place_id {
        None => false,
        Some(id) => store.place_by_id(id)?.is_some(),
    };
    crate::application::place_policy::check_place_policy(
        place_policy,
        visibility == Visibility::Direct,
        in_reply_to.is_some(),
        place_id,
        place_resolves,
    )
}

/// Core extension CORE-L: under `enforced`, a top-level non-DM status
/// carrying media needs an effective license (per-attachment else author
/// default) on every attachment. Replies and DMs are exempt; the media
/// upload itself never gated. Missing rows are left to the store's
/// attachability check below (same 422 family).
pub(super) fn check_license_gate(
    store: &dyn Store,
    license_policy: &str,
    visibility: Visibility,
    in_reply_to: Option<Id>,
    cmd: &CreateStatusCommand,
    account: &Account,
) -> Result<()> {
    if license_policy == "enforced"
        && visibility != Visibility::Direct
        && in_reply_to.is_none()
        && !cmd.media_ids.is_empty()
    {
        let attached = store.media_by_ids(&cmd.media_ids)?;
        let mut effective = Vec::with_capacity(cmd.media_ids.len());
        for id in &cmd.media_ids {
            if let Some(media) = attached.get(id) {
                effective.push(crate::application::license_policy::effective_license(
                    media.license,
                    account.default_license,
                ));
            }
        }
        if effective.len() == cmd.media_ids.len() {
            crate::application::license_policy::check_status_licenses(
                license_policy,
                false,
                false,
                true,
                &effective,
            )?;
        }
    }
    Ok(())
}

/// Core extension CORE-T: under `block`, a top-level or reply write leaving
/// an image/video/gifv attachment without a description is a 422 (local
/// writes only; text-only posts unaffected). Missing rows are left to the
/// store's attachability check (same 422 family).
pub(super) fn check_alt_text_gate(
    store: &dyn Store,
    alt_text_policy: &str,
    cmd: &CreateStatusCommand,
) -> Result<()> {
    if matches!(alt_text_policy, "nag" | "block") && !cmd.media_ids.is_empty() {
        let attached = store.media_by_ids(&cmd.media_ids)?;
        let mut pairs = Vec::with_capacity(cmd.media_ids.len());
        for id in &cmd.media_ids {
            if let Some(media) = attached.get(id) {
                pairs.push((media.r#type.clone(), media.description.clone()));
            }
        }
        if pairs.len() == cmd.media_ids.len() {
            crate::application::alt_text_policy::check_block(alt_text_policy, &pairs)?;
        }
    }
    Ok(())
}

/// Build the store input from computed stages plus the leftover owned
/// command tail. The quoted author's silent mention is appended by the store
/// from `NewQuoteSpec.quoted_account_id`, never via `mentions`.
pub(super) fn build_new_status(account: &Account, base_url: &str, parts: StatusParts) -> NewStatus {
    let StatusParts {
        account_id,
        text,
        spoiler,
        sensitive,
        visibility,
        language,
        in_reply_to,
        in_reply_to_account,
        mentions,
        tags,
        media_ids,
        place_id,
        poll,
        quote,
        application_name,
        application_website,
        idempotency_key,
    } = parts;
    let base = base_url.trim_end_matches('/');
    let content_html = render_html(&text);
    NewStatus {
        remote: None,
        account_id,
        uri_base: format!("{base}/users/{}", account.username),
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
    }
}

/// Fire-and-forget `interactions` journal write for the reply case.
///
/// D39 `interactions` journal: upstream counts ONLY non-self replies
/// (`PostStatusService#bump_potential_friendship!`,
/// `app/services/post_status_service.rb:262-266`). Idempotent replays
/// (`replayed`) and top-level/self-reply posts journal nothing. A journal
/// failure never fails the post itself (metrics must not break the write
/// path — same fire-and-forget shape as `record_sign_in` at
/// `api/oauth/authorization.rs:272`).
pub(super) fn journal_reply(
    store: &dyn Store,
    replayed: bool,
    in_reply_to: Option<Id>,
    in_reply_to_account: Option<Id>,
    account_id: Id,
    now_ms: u64,
) {
    if replayed || in_reply_to.is_none() || in_reply_to_account == Some(account_id) {
        return;
    }
    let _ = store.record_interaction(InteractionEvent::for_journal(
        account_id,
        InteractionEventKind::Reply,
        now_ms,
    ));
}
