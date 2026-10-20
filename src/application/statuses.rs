//! Status use cases: composing, showing, editing, deleting and reading
//! threads and edit history.
//!
//! Every read goes through the shared visibility policy
//! (`application::policy::can_fetch`) with the viewer/author `PairState`
//! computed by the `RelationshipStore`; policy logic is never re-implemented
//! here. Validation error strings mirror pinned Mastodon v4.7.3 messages
//! (`config/locales/en.yml` and the validators under `app/validators/`).

use std::collections::{HashSet, VecDeque};
use std::sync::OnceLock;

use regex::Regex;

use crate::application::accounts::host_from_url;
use crate::application::ports::{
    AccountStatusesQuery, DeletedStatus, NewPollSpec, NewQuoteSpec, NewStatus, PairState,
    StatusUpdate, Store,
};
use crate::application::policy;
use crate::domain::account::normalize_username;
use crate::domain::{
    Error, Id, QuotePolicy, Result, Status, StatusApplication, StatusVersion, Visibility,
};

// ---------------------------------------------------------------------------
// Mastodon-compatible limits and verbatim error strings (v4.7.3, pinned).
// ---------------------------------------------------------------------------

/// Default status text limit (`StatusLengthValidator::MAX_CHARS`); URLs count
/// as a fixed 23 characters (see [`count_length`]). Configurable per server
/// up to [`ABS_MAX_STATUS_LENGTH`] via `--max-status-chars=`.
pub const DEFAULT_MAX_STATUS_LENGTH: usize = 500;
/// Hard ceiling on the configurable status text limit (Misskey parity).
pub const ABS_MAX_STATUS_LENGTH: usize = 3000;
/// Every URL counts as this many characters (`URL_PLACEHOLDER`).
const URL_LENGTH: usize = 23;
/// Mastodon's `Status::MEDIA_ATTACHMENTS_LIMIT` for a single status.
const MAX_MEDIA_ATTACHMENTS: usize = 4;
/// Poll options: `PollOptionsValidator` bounds.
const MIN_POLL_OPTIONS: usize = 2;
const MAX_POLL_OPTIONS: usize = 4;
const MAX_POLL_OPTION_CHARS: usize = 50;
/// Poll expiry: `PollExpirationValidator` bounds (5 minutes .. 1.month).
const MIN_POLL_EXPIRY_S: u64 = 300;
const MAX_POLL_EXPIRY_S: u64 = 2_592_000;
/// Per-parent children fetch while walking a thread's descendants.
const CHILDREN_PER_LEVEL: usize = 200;

const ERR_BLANK_TEXT: &str = "Validation failed: Text can't be blank";
/// Prefix of the over-long-text error; the configured limit is interpolated
/// (Mastodon embeds the number: "character limit of 500 exceeded").
const ERR_TEXT_TOO_LONG_PREFIX: &str = "Validation failed: character limit of ";
/// Over-long-text error with the configured limit interpolated, matching
/// Mastodon's message shape (`character limit of 500 exceeded` at default).
pub fn text_too_long_error(max_chars: usize) -> String {
    format!("{ERR_TEXT_TOO_LONG_PREFIX}{max_chars} exceeded")
}
const ERR_VISIBILITY: &str =
    "Validation failed: Visibility must be public, unlisted, private or direct";
const ERR_LANGUAGE: &str = "Validation failed: Language is invalid";
const ERR_TOO_MANY_MEDIA: &str = "Cannot attach more than 4 files";
const ERR_RECORD_INVALID: &str = "Record invalid";
const ERR_QUOTE_NOT_MENTIONED: &str =
    "Validation failed: Cannot quote a non-mentioned user in a Private Mention post.";
const ERR_POLL_TOO_FEW: &str = "Validation failed: Choices must have more than one item";
const ERR_POLL_TOO_MANY: &str = "Validation failed: Choices can't contain more than 4 items";
const ERR_POLL_OPTION_TOO_LONG: &str =
    "Validation failed: Choices cannot be longer than 50 characters each";
const ERR_POLL_DUPLICATE_OPTIONS: &str = "Validation failed: Choices contain duplicate items";
const ERR_POLL_EXPIRY_TOO_SHORT: &str = "Validation failed: Deadline is too soon";
const ERR_POLL_EXPIRY_TOO_LONG: &str = "Validation failed: Deadline is too far into the future";

/// 404 body for an unfetchable/unknown reply parent (Mastodon
/// `statuses.errors.in_reply_not_found`).
const ERR_REPLY_NOT_FOUND: &str =
    "The post you are trying to reply to does not appear to exist.";
/// 404 body for an unquotable/unknown quoted status
/// (`statuses.errors.quoted_status_not_found`).
const ERR_QUOTE_NOT_FOUND: &str = "The post you are trying to quote does not appear to exist.";

// ---------------------------------------------------------------------------
// Commands and results.
// ---------------------------------------------------------------------------

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

/// Input for `PUT /api/v1/statuses/:id`.
///
/// `None` fields are left unchanged.
#[derive(Debug, Clone)]
pub struct UpdateStatusCommand {
    pub account_id: Id,
    pub status_id: Id,
    pub text: Option<String>,
    pub sensitive: Option<bool>,
    pub spoiler_text: Option<String>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<Id>>,
    pub poll: Option<PollCommand>,
    /// Wire value, validated here when present.
    pub quote_approval_policy: Option<String>,
}

/// `GET /api/v1/statuses/:id/context`: the status's thread around the root.
#[derive(Debug, Clone)]
pub struct ContextTree {
    /// Reply chain up to the root, oldest first.
    pub ancestors: Vec<Status>,
    /// Replies below the root, breadth-first (parent's children ahead of
    /// deeper levels, mirroring Mastodon's thread order).
    pub descendants: Vec<Status>,
}

// ---------------------------------------------------------------------------
// Composition.
// ---------------------------------------------------------------------------

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
    let spoiler = spoiler_text.and_then(|s| {
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    });
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
    let tags = extract_tags(&text);
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

    Ok(CreateStatusResult {
        status: created.status,
        replayed: created.replayed,
    })
}

/// Validate a poll command and turn it into the store's `NewPollSpec`
/// (`expires_at_ms` is absolute: `now + expires_in_s`).
///
/// Messages mirror `PollOptionsValidator` / `PollExpirationValidator` wrapped
/// in `ActiveRecord::RecordInvalid`'s "Validation failed: " prefix.
fn build_poll_spec(now_ms: u64, cmd: PollCommand) -> Result<NewPollSpec> {
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

// ---------------------------------------------------------------------------
// Reads.
// ---------------------------------------------------------------------------

/// `GET /api/v1/statuses/:id` — one status under the show gate.
///
/// Missing, deleted and non-visible statuses are indistinguishable:
/// `NotFound("status")`, exactly like Mastodon's status sub-routes.
pub fn show_status(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let status = store
        .status_by_id(status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    if !fetchable(store, viewer, &status, now_ms)? {
        return Err(Error::NotFound("status".into()));
    }
    Ok(status)
}

/// `GET /api/v1/statuses/:id/source` — the raw source of a status under the
/// same visibility gate as [`show_status`].
pub fn status_source(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    show_status(store, viewer, status_id, now_ms)
}

/// `GET /api/v1/statuses/:id/context` — ancestors and descendants of a
/// status under the show gate (`NotFound("status")` when the root is
/// invisible).
///
/// Authenticated callers get the near-unlimited Mastodon context (4096
/// ancestors, 4096 descendants, unbounded depth); anonymous callers get 40
/// ancestors and 60 descendants within depth 20. The caps bound the ids
/// *loaded* (Mastodon applies its SQL `LIMIT` before permission filtering),
/// so an invisible ancestor/descendant is skipped while the walk continues.
/// Descendants are collected breadth-first; each parent's children are read
/// newest-first, at most 200 per level.
pub fn status_context(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
    authenticated: bool,
) -> Result<ContextTree> {
    let root = show_status(store, viewer, status_id, now_ms)?;
    let (ancestor_cap, descendant_cap, depth_limit) = if authenticated {
        (4_096usize, 4_096usize, None)
    } else {
        (40usize, 60usize, Some(20usize))
    };

    let ancestors = collect_ancestors(store, viewer, &root, now_ms, ancestor_cap)?;
    let descendants =
        collect_descendants(store, viewer, &root, now_ms, descendant_cap, depth_limit)?;
    Ok(ContextTree {
        ancestors,
        descendants,
    })
}

/// Walk `in_reply_to_id` upward, oldest-last, then reverse to oldest-first.
fn collect_ancestors(
    store: &dyn Store,
    viewer: Option<Id>,
    root: &Status,
    now_ms: u64,
    cap: usize,
) -> Result<Vec<Status>> {
    let mut visited: HashSet<Id> = HashSet::new();
    visited.insert(root.id);
    let mut chain: Vec<Status> = Vec::new();
    let mut cursor = root.in_reply_to_id;
    let mut walked = 0usize;

    while let Some(parent_id) = cursor {
        if walked >= cap {
            break;
        }
        walked += 1;
        let Some(parent) = store.status_by_id(parent_id)? else {
            break;
        };
        // Cycle guard on corrupt reply chains.
        if !visited.insert(parent.id) {
            break;
        }
        cursor = parent.in_reply_to_id;
        // Invisible ancestors are excluded, but the chain keeps walking.
        if fetchable(store, viewer, &parent, now_ms)? {
            chain.push(parent);
        }
    }
    chain.reverse();
    Ok(chain)
}

/// Breadth-first descent of the reply tree; stops at the total cap or the
/// depth limit (unlimited for authenticated callers).
fn collect_descendants(
    store: &dyn Store,
    viewer: Option<Id>,
    root: &Status,
    now_ms: u64,
    cap: usize,
    depth_limit: Option<usize>,
) -> Result<Vec<Status>> {
    let mut visited: HashSet<Id> = HashSet::new();
    visited.insert(root.id);
    let mut queue: VecDeque<(Id, usize)> = VecDeque::new();
    queue.push_back((root.id, 0));
    let mut descendants: Vec<Status> = Vec::new();
    let mut loaded = 0usize;

    while let Some((parent_id, depth)) = queue.pop_front() {
        if descendants.len() >= cap || loaded >= cap {
            break;
        }
        if depth_limit.is_some_and(|max| depth >= max) {
            continue;
        }
        for child in store.status_children(parent_id, None, CHILDREN_PER_LEVEL)? {
            if !visited.insert(child.id) {
                continue;
            }
            loaded += 1;
            queue.push_back((child.id, depth + 1));
            // Invisible/deleted children are skipped; their replies below
            // them are still walked.
            if fetchable(store, viewer, &child, now_ms)? {
                descendants.push(child);
                if descendants.len() >= cap {
                    break;
                }
            }
            if loaded >= cap {
                break;
            }
        }
    }
    Ok(descendants)
}

/// `GET /api/v1/statuses/:id/history` — the status's edit snapshots, oldest
/// first, under the show gate.
///
/// A never-edited status has no stored snapshots, so a version-0 snapshot is
/// synthesized from the current record (timestamped by `edited_at_ms` when
/// present, otherwise `created_at_ms`).
pub fn status_history(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Vec<StatusVersion>> {
    let status = show_status(store, viewer, status_id, now_ms)?;
    let mut versions = store.status_versions(status_id)?;
    if versions.is_empty() {
        let poll_options = store
            .poll_by_status(status_id)?
            .map(|poll| poll.options.into_iter().map(|o| o.title).collect());
        versions.push(StatusVersion {
            status_id,
            version: 0,
            account_id: status.account_id,
            content_html: status.content_html.clone(),
            source_text: status.source_text.clone(),
            spoiler_text: status.spoiler_text.clone(),
            sensitive: status.sensitive,
            media_ids: status.media_ids.clone(),
            poll_options,
            created_at_ms: status.edited_at_ms.unwrap_or(status.created_at_ms),
        });
    }
    Ok(versions)
}

// ---------------------------------------------------------------------------
// Editing and deletion.
// ---------------------------------------------------------------------------

/// `PUT /api/v1/statuses/:id` — edit an owned status.
///
/// Missing, deleted and non-owned statuses are indistinguishable
/// (`NotFound("status")`, matching Mastodon's `Status.where(account:).
/// find`). Returns `Ok(None)` when the patch changed nothing (no edit
/// snapshot, no `edited_at` bump). Note the deliberate Mastodon-shaped
/// asymmetries: an unknown `language` or `quote_approval_policy` is a 422
/// while text length/blankness are re-validated by the store-side rules only
/// at creation; a supplied poll is validated exactly as on creation.
pub fn update_status(
    store: &dyn Store,
    now_ms: u64,
    cmd: UpdateStatusCommand,
) -> Result<Option<Status>> {
    let status = store
        .status_by_id(cmd.status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    if status.is_deleted() || !policy::is_owner(cmd.account_id, &status) {
        return Err(Error::NotFound("status".into()));
    }

    let mut update = StatusUpdate::default();
    if let Some(text) = cmd.text {
        update.source_text = Some(text.clone());
        update.content_html = Some(render_html(&text));
    }
    if let Some(spoiler) = cmd.spoiler_text {
        // Present-and-blank clears the CW (`Some(None)`), as in Mastodon.
        update.spoiler_text = Some(if spoiler.trim().is_empty() {
            None
        } else {
            Some(spoiler)
        });
    }
    if let Some(sensitive) = cmd.sensitive {
        update.sensitive = Some(sensitive);
    }
    if let Some(language) = cmd.language {
        if !language_regex().is_match(&language) {
            return Err(Error::InvalidInput(ERR_LANGUAGE.into()));
        }
        update.language = Some(language);
    }
    if let Some(media_ids) = cmd.media_ids {
        update.media_ids = Some(media_ids);
    }
    if let Some(poll) = cmd.poll {
        update.poll = Some(build_poll_spec(now_ms, poll)?);
    }
    if let Some(raw) = cmd.quote_approval_policy {
        update.quote_approval_policy = Some(
            QuotePolicy::from_wire(&raw)
                .ok_or_else(|| Error::InvalidInput(ERR_RECORD_INVALID.into()))?,
        );
    }

    store.update_status(cmd.status_id, cmd.account_id, update, now_ms)
}

/// `DELETE /api/v1/statuses/:id` — tombstone a status, returning the
/// pre-deletion record. Ownership lives in the store: missing, deleted and
/// non-owned all answer `NotFound`.
pub fn delete_status(
    store: &dyn Store,
    viewer: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<DeletedStatus> {
    store.delete_status(status_id, viewer, now_ms)
}

// ---------------------------------------------------------------------------
// Collections.
// ---------------------------------------------------------------------------

/// Mastodon's multi-fetch (`GET /api/v1/statuses?ids[]=`): non-deleted
/// statuses among `ids`, id-descending, silently omitting anything the
/// viewer may not fetch.
pub fn statuses_by_ids(
    store: &dyn Store,
    viewer: Option<Id>,
    ids: &[Id],
    now_ms: u64,
) -> Result<Vec<Status>> {
    // Mastodon deduplicates the requested ids (`status_ids` = `.uniq`).
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let rows = store.statuses_by_ids(&ids)?;
    let mut visible = Vec::with_capacity(rows.len());
    for status in rows {
        if fetchable(store, viewer, &status, now_ms)? {
            visible.push(status);
        }
    }
    Ok(visible)
}

/// `GET /api/v1/accounts/:id/statuses` — one account's statuses under the
/// query's cursors/filters, then per-viewer filtering.
///
/// A missing account is `NotFound` (404). An anonymous viewer sees only
/// distributable statuses: `can_fetch` with the default `PairState` admits
/// `public`/`unlisted` and rejects `private`/`direct`.
pub fn account_statuses(
    store: &dyn Store,
    viewer: Option<Id>,
    account_id: Id,
    query: AccountStatusesQuery,
    now_ms: u64,
) -> Result<Vec<Status>> {
    if store.account_by_id(account_id)?.is_none() {
        return Err(Error::NotFound("status".into()));
    }
    let rows = store.statuses_by_account(account_id, query)?;
    let mut visible = Vec::with_capacity(rows.len());
    for status in rows {
        if fetchable(store, viewer, &status, now_ms)? {
            visible.push(status);
        }
    }
    Ok(visible)
}

// ---------------------------------------------------------------------------
// Shared gates.
// ---------------------------------------------------------------------------

/// `can_fetch(viewer, status, pair_state(author, viewer))` for one status.
fn fetchable(store: &dyn Store, viewer: Option<Id>, status: &Status, now_ms: u64) -> Result<bool> {
    let pair = match viewer {
        Some(viewer) => store.pair_state(status.account_id, viewer, now_ms)?,
        None => PairState::default(),
    };
    Ok(policy::can_fetch(viewer, status, pair))
}

// ---------------------------------------------------------------------------
// Rendering and extraction helpers.
// ---------------------------------------------------------------------------

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

/// Extract lowercased hashtag bodies (`[A-Za-z0-9_]+` after `#`), skipping
/// `#` runs that sit inside a word (`café#tag`), preserving order and
/// deduplicating (Mastodon stores unique tags per status).
pub fn extract_tags(text: &str) -> Vec<String> {
    let re = tag_regex();
    let mut tags: Vec<String> = Vec::new();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("tag match");
        let before = text[..whole.start()].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let tag = caps.get(1).expect("tag body").as_str().to_ascii_lowercase();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// Extract mention handles (`@user` or `@user@domain`) that start a line or
/// follow whitespace or `>`, so email addresses are not picked up. Returns
/// the raw handle bodies in order of appearance, deduplicated (case-
/// insensitive dedup happens at id resolution; see [`resolve_mentions`]).
pub fn extract_mention_handles(text: &str) -> Vec<String> {
    let re = mention_regex();
    let mut handles: Vec<String> = Vec::new();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("mention match");
        let before = text[..whole.start()].chars().next_back();
        if !before.is_none_or(|c| c.is_whitespace() || c == '>') {
            continue;
        }
        let handle = caps.get(1).expect("mention body").as_str().to_string();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

/// Resolve mention handles to account ids via
/// `account_by_handle(normalized, domain)`; bare `@user` handles use
/// `local_domain`. Unknown handles are dropped — this is a local-only server
/// (remote phantom accounts are Phase 6) — and duplicate ids are collapsed
/// while preserving mention order.
pub fn resolve_mentions(
    store: &dyn Store,
    handles: &[String],
    local_domain: Option<&str>,
) -> Vec<Id> {
    let mut ids: Vec<Id> = Vec::new();
    for handle in handles {
        let (username, domain) = match handle.split_once('@') {
            Some((user, domain)) => (user, domain),
            None => (handle.as_str(), local_domain.unwrap_or("")),
        };
        if username.is_empty() || domain.is_empty() {
            continue;
        }
        match store.account_by_handle(&normalize_username(username), domain) {
            Ok(Some(account)) => {
                if !ids.contains(&account.id) {
                    ids.push(account.id);
                }
            }
            // Unknown handle or store error: dropped, never phantom-created.
            Ok(None) | Err(_) => {}
        }
    }
    ids
}

/// Render plain source text as sanitized HTML: HTML-escaped, paragraphs
/// split on blank lines, single newlines become `<br />`, wrapped in
/// `<p>...</p>`. Mention anchors are deliberately not included (the plain
/// text renderer stays deterministic); blank input renders to "" (a
/// media-only status has empty content, as in Mastodon).
pub fn render_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }

    let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            if !paragraphs.last().expect("paragraph").is_empty() {
                paragraphs.push(Vec::new());
            }
        } else {
            paragraphs.last_mut().expect("paragraph").push(line);
        }
    }

    let mut html = String::new();
    for paragraph in paragraphs {
        if paragraph.is_empty() {
            continue;
        }
        html.push_str("<p>");
        for (i, line) in paragraph.iter().enumerate() {
            if i > 0 {
                html.push_str("<br />");
            }
            html.push_str(&escape_html(line));
        }
        html.push_str("</p>");
    }
    html
}

/// Escape the five HTML-significant characters (`&` first).
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Compiled patterns.
// ---------------------------------------------------------------------------

fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"https?://\S+").expect("valid URL regex"))
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#([A-Za-z0-9_]+)").expect("valid tag regex"))
}

fn mention_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"@([A-Za-z0-9_]+(?:@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)?)")
            .expect("valid mention regex")
    })
}

/// Normalize a hashtag wire value the way tags are stored (lowercased,
/// `#`-stripped, invalid characters removed) — mirrors Mastodon's
/// `HashtagNormalizer` for the local profile.
pub fn normalize_tag(raw: &str) -> String {
    raw.trim_start_matches('#')
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Custom-emoji shortcode tokens (`:shortcode:`) referenced by a text.
///
/// Mirrors `CustomEmoji.from_text`: the token list drives the payload's
/// `emojis` array; the text itself keeps the literal tokens (clients
/// substitute them).
pub fn extract_emoji_shortcodes(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r":([A-Za-z0-9_]{2,}):").expect("valid emoji shortcode regex")
    });
    re.captures_iter(text)
        .map(|cap| cap[1].to_string())
        .collect()
}

fn language_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\A[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*\z").expect("valid language regex")
    })
}
