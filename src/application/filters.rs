//! Filter use cases (Phase 4): v2 CRUD, the v1 synthesized projection, and
//! page-level match filtering.
//!
//! Mirrors Mastodon's `Api::V1::FiltersController`, `Api::V2::FiltersController`
//! and the `Filters::Keywords`/`Filters::Statuses` sub-resources: one stored
//! v2 model (`CustomFilter` + keyword/status rules) serves both APIs, with the
//! v1 API projecting one *keyword* row per "filter". Validation uses the
//! exact Mastodon en.yml 422 messages; ownership gaps answer the explicit
//! `Record not found` 404.
//!
//! The `filtered`-array matcher ([`filter_results`]) is the page-level hook
//! for timelines/notifications: those endpoints (wired by the integrator, not
//! this module) collect a page of statuses and then call `filter_results`
//! once per page to fill each status's `filtered` array (copying the entries
//! onto reblog wrappers as this function already does for reblog rows).

use std::collections::HashMap;

use regex::RegexBuilder;

use crate::application::policy;
use crate::application::ports::{FilterBundle, FilterPatch, NewFilter, Store, filter_result_json};
use crate::domain::{CustomFilter, Error, FilterAction, FilterKeyword, FilterStatus, Id, Result, Status};

/// Mastodon en.yml validation messages (exact strings, 422 bodies).
pub const ERR_TITLE_BLANK: &str = "Validation failed: Title can't be blank";
pub const ERR_TITLE_TOO_LONG: &str =
    "Validation failed: Title is too long (maximum is 256 characters)";
pub const ERR_CONTEXT: &str = "Validation failed: Context is not included in the list";
pub const ERR_ACTION: &str = "Validation failed: Action is not included in the list";
pub const ERR_EXPIRES: &str = "Validation failed: Expires in is not a number";
pub const ERR_KEYWORD_BLANK: &str = "Validation failed: Keyword can't be blank";
pub const ERR_KEYWORD_TOO_LONG: &str =
    "Validation failed: Keyword is too long (maximum is 500 characters)";
/// The v1 multi-keyword parent-attribute guard (422, no `Validation failed:`
/// prefix — Mastodon raises a bare `Mastodon::ValidationError`).
pub const ERR_V1_MULTI: &str = "These parameters cannot be changed from this application \
because they apply to more than one filter keyword. Use a more recent application or the \
web interface.";

const RECORD_NOT_FOUND: &str = "Record not found";

fn not_found() -> Error {
    Error::NotFound(RECORD_NOT_FOUND.into())
}

/// Seconds-from-now expiry -> absolute ms; `None`/`0` = never (Mastodon's
/// `expires_in=0`/blank clears the expiry).
fn expiry_ms(expires_in_s: Option<u64>, now_ms: u64) -> Option<u64> {
    expires_in_s
        .filter(|s| *s > 0)
        .map(|s| now_ms.saturating_add(s.saturating_mul(1_000)))
}

fn validate_title(title: &str) -> Result<()> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(ERR_TITLE_BLANK.into()));
    }
    if trimmed.chars().count() > CustomFilter::TITLE_LENGTH_LIMIT {
        return Err(Error::InvalidInput(ERR_TITLE_TOO_LONG.into()));
    }
    Ok(())
}

fn validate_context(context: &[String]) -> Result<()> {
    if context.is_empty()
        || context
            .iter()
            .any(|value| !CustomFilter::VALID_CONTEXTS.contains(&value.as_str()))
    {
        return Err(Error::InvalidInput(ERR_CONTEXT.into()));
    }
    Ok(())
}

fn validate_action(action: &str) -> Result<FilterAction> {
    FilterAction::from_wire(action).ok_or_else(|| Error::InvalidInput(ERR_ACTION.into()))
}

fn validate_keyword(keyword: &str) -> Result<()> {
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(ERR_KEYWORD_BLANK.into()));
    }
    if trimmed.chars().count() > FilterKeyword::KEYWORD_LENGTH_LIMIT {
        return Err(Error::InvalidInput(ERR_KEYWORD_TOO_LONG.into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// v2 CRUD.
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters` — the viewer's filters (optionally narrowed to one
/// context).
pub fn index(store: &dyn Store, viewer: Id, context: Option<&str>) -> Result<Vec<FilterBundle>> {
    store.filters_by_account(viewer, context)
}

/// `POST /api/v2/filters` — create a filter with its initial keywords.
///
/// `expires_in_s` is the parsed `expires_in` param (absent/`0` = never);
/// `action` is the wire `filter_action` (blank/absent defaults to `warn` in
/// the adapter).
pub fn create(
    store: &dyn Store,
    viewer: Id,
    title: &str,
    context: &[String],
    expires_in_s: Option<u64>,
    action: &str,
    keywords: Vec<(String, bool)>,
    now_ms: u64,
) -> Result<FilterBundle> {
    validate_title(title)?;
    validate_context(context)?;
    let action = validate_action(action)?;
    let keywords: Vec<(String, bool)> = keywords
        .into_iter()
        .map(|(word, whole_word)| {
            validate_keyword(&word)?;
            Ok((word.trim().to_string(), whole_word))
        })
        .collect::<Result<_>>()?;
    store.create_filter(
        NewFilter {
            account_id: viewer,
            title: title.trim().to_string(),
            context: context.to_vec(),
            expires_at_ms: expiry_ms(expires_in_s, now_ms),
            action,
            keywords,
        },
        now_ms,
    )
}

/// `GET /api/v2/filters/:id` — the viewer's filter (404 otherwise).
pub fn show(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterBundle> {
    owned_filter(store, viewer, id)
}

/// One `keywords_attributes[]` entry as parsed by the API adapter.
#[derive(Debug, Clone, Default)]
pub struct KeywordAttr {
    pub id: Option<Id>,
    pub keyword: Option<String>,
    pub whole_word: Option<bool>,
    pub destroy: bool,
}

/// A v2 update; every field `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct FilterUpdate {
    pub title: Option<String>,
    pub context: Option<Vec<String>>,
    /// `Some(None)` clears the expiry; `None` leaves it unchanged.
    pub expires_in_s: Option<Option<u64>>,
    pub action: Option<String>,
    /// `keywords_attributes` replacements (`None` id = add; `destroy` drops).
    pub keywords: Option<Vec<KeywordAttr>>,
}

/// `PUT/PATCH /api/v2/filters/:id` — patch the viewer's filter.
pub fn update(
    store: &dyn Store,
    viewer: Id,
    id: Id,
    update: FilterUpdate,
    now_ms: u64,
) -> Result<FilterBundle> {
    // Ownership first (Mastodon's `set_filter` 404s before validation).
    let bundle = owned_filter(store, viewer, id)?;
    // Validate everything before mutating.
    if let Some(title) = &update.title {
        validate_title(title)?;
    }
    if let Some(context) = &update.context {
        validate_context(context)?;
    }
    let action = update
        .action
        .as_deref()
        .map(validate_action)
        .transpose()?;
    let keywords = update
        .keywords
        .map(|entries| translate_keyword_attrs(entries, &bundle))
        .transpose()?;
    let patch = FilterPatch {
        title: update.title.map(|title| title.trim().to_string()),
        context: update.context,
        expires_at_ms: update
            .expires_in_s
            .map(|expires| expiry_ms(expires, now_ms)),
        action,
        keywords,
    };
    store
        .update_filter(viewer, id, patch, now_ms)?
        .ok_or_else(not_found)?;
    // `update_filter`'s returned bundle can echo pre-commit keyword rows (its
    // read snapshot opens before the commit); re-read for the exact response.
    store.filter_by_id(id)?.ok_or_else(not_found)
}

/// Resolve `keywords_attributes` rows against the current keyword set:
/// entries with an id but no keyword/whole_word inherit the stored values
/// (Mastodon's Active Record behaviour), id-less `_destroy` rows are dropped.
fn translate_keyword_attrs(
    entries: Vec<KeywordAttr>,
    bundle: &FilterBundle,
) -> Result<Vec<(Option<Id>, String, bool, bool)>> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let existing = entry
            .id
            .and_then(|id| bundle.keywords.iter().find(|keyword| keyword.id == id));
        if entry.destroy {
            if entry.id.is_some() {
                out.push((entry.id, String::new(), false, true));
            }
            continue;
        }
        let word = entry
            .keyword
            .map(|word| word.trim().to_string())
            .unwrap_or_else(|| existing.map(|k| k.keyword.clone()).unwrap_or_default());
        validate_keyword(&word)?;
        let whole_word = entry
            .whole_word
            .or_else(|| existing.map(|k| k.whole_word))
            .unwrap_or(false);
        out.push((entry.id, word, whole_word, false));
    }
    Ok(out)
}

/// `DELETE /api/v2/filters/:id` — delete the viewer's filter (and its rules).
pub fn destroy(store: &dyn Store, viewer: Id, id: Id) -> Result<()> {
    owned_filter(store, viewer, id)?;
    store.delete_filter(viewer, id)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Keyword sub-resource (`/api/v2/filters/.../keywords`).
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters/:filter_id/keywords` — the filter's keywords.
pub fn keyword_index(store: &dyn Store, viewer: Id, filter_id: Id) -> Result<Vec<FilterKeyword>> {
    Ok(owned_filter(store, viewer, filter_id)?.keywords)
}

/// `POST /api/v2/filters/:filter_id/keywords` — add a keyword.
pub fn keyword_create(
    store: &dyn Store,
    viewer: Id,
    filter_id: Id,
    keyword: &str,
    whole_word: bool,
    now_ms: u64,
) -> Result<FilterKeyword> {
    owned_filter(store, viewer, filter_id)?;
    validate_keyword(keyword)?;
    store
        .create_keyword(viewer, filter_id, keyword.trim(), whole_word, now_ms)?
        .ok_or_else(not_found)
}

/// `GET /api/v2/filters/keywords/:id` — one keyword the viewer owns.
pub fn keyword_show(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterKeyword> {
    Ok(owned_keyword(store, viewer, id)?.0)
}

/// `PUT/PATCH /api/v2/filters/keywords/:id` — patch a keyword.
pub fn keyword_update(
    store: &dyn Store,
    viewer: Id,
    id: Id,
    keyword: Option<&str>,
    whole_word: Option<bool>,
    now_ms: u64,
) -> Result<FilterKeyword> {
    owned_keyword(store, viewer, id)?;
    if let Some(word) = keyword {
        validate_keyword(word)?;
    }
    store
        .update_keyword(viewer, id, keyword.map(str::trim), whole_word, now_ms)?
        .ok_or_else(not_found)
}

/// `DELETE /api/v2/filters/keywords/:id` — delete a keyword (the parent
/// filter row stays).
pub fn keyword_destroy(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterKeyword> {
    owned_keyword(store, viewer, id)?;
    store.delete_keyword(viewer, id)?.ok_or_else(not_found)
}

// ---------------------------------------------------------------------------
// Status rule sub-resource (`/api/v2/filters/.../statuses`).
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters/:filter_id/statuses` — the filter's status rules.
pub fn status_index(store: &dyn Store, viewer: Id, filter_id: Id) -> Result<Vec<FilterStatus>> {
    owned_filter(store, viewer, filter_id)?;
    store.filter_statuses(filter_id)
}

/// `POST /api/v2/filters/:filter_id/statuses` — add a status rule
/// (`status_id`); the status must exist and be fetchable by the viewer.
pub fn status_create(
    store: &dyn Store,
    viewer: Id,
    filter_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<FilterStatus> {
    owned_filter(store, viewer, filter_id)?;
    let status = store.status_by_id(status_id)?.ok_or_else(not_found)?;
    let pair = store.pair_state(status.account_id, viewer, now_ms)?;
    if !policy::can_fetch(Some(viewer), &status, pair) {
        return Err(not_found());
    }
    store
        .create_filter_status(viewer, filter_id, status_id, now_ms)?
        .ok_or_else(not_found)
}

/// `GET /api/v2/filters/statuses/:id` — one status rule the viewer owns.
///
/// The port set has no by-id rule read, so the lookup scopes through the
/// viewer's own filters (bounded scans, same 404 for foreign rows).
pub fn status_show(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterStatus> {
    for bundle in store.filters_by_account(viewer, None)? {
        if let Some(rule) = bundle.statuses.into_iter().find(|rule| rule.id == id) {
            return Ok(rule);
        }
    }
    Err(not_found())
}

/// `DELETE /api/v2/filters/statuses/:id` — delete a status rule.
pub fn status_destroy(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterStatus> {
    store
        .delete_filter_status(viewer, id)?
        .ok_or_else(not_found)
}

// ---------------------------------------------------------------------------
// v1 projection (each v1 "filter" is one keyword row).
// ---------------------------------------------------------------------------

/// One synthesized v1 filter row: the keyword, its parent filter and the
/// filter's status-rule ids (the inputs of `REST::V1::FilterSerializer`).
#[derive(Debug, Clone)]
pub struct V1Row {
    pub keyword: FilterKeyword,
    pub filter: CustomFilter,
    pub status_ids: Vec<Id>,
}

fn v1_row(bundle: &FilterBundle, keyword: FilterKeyword) -> V1Row {
    V1Row {
        keyword,
        filter: bundle.filter.clone(),
        status_ids: bundle.statuses.iter().map(|rule| rule.status_id).collect(),
    }
}

/// `GET /api/v1/filters` — every keyword row of the viewer's filters.
pub fn v1_index(store: &dyn Store, viewer: Id) -> Result<Vec<V1Row>> {
    let mut rows = Vec::new();
    for bundle in store.filters_by_account(viewer, None)? {
        for keyword in &bundle.keywords {
            rows.push(v1_row(&bundle, keyword.clone()));
        }
    }
    Ok(rows)
}

/// `POST /api/v1/filters` — create a filter + its single keyword in ONE
/// transaction (`title = phrase`; `irreversible` maps to `hide`).
pub fn v1_create(
    store: &dyn Store,
    viewer: Id,
    phrase: &str,
    context: &[String],
    expires_in_s: Option<u64>,
    irreversible: bool,
    whole_word: bool,
    now_ms: u64,
) -> Result<V1Row> {
    let action = if irreversible {
        FilterAction::Hide
    } else {
        FilterAction::Warn
    };
    let bundle = create(
        store,
        viewer,
        phrase,
        context,
        expires_in_s,
        action.as_str(),
        vec![(phrase.to_string(), whole_word)],
        now_ms,
    )?;
    let keyword = bundle
        .keywords
        .first()
        .cloned()
        .ok_or_else(|| Error::Internal("v1 filter created without a keyword".into()))?;
    Ok(v1_row(&bundle, keyword))
}

/// `GET /api/v1/filters/:id` — the v1 row for a keyword id.
pub fn v1_show(store: &dyn Store, viewer: Id, keyword_id: Id) -> Result<V1Row> {
    let (keyword, filter) = owned_keyword(store, viewer, keyword_id)?;
    let status_ids = store
        .filter_statuses(filter.id)?
        .into_iter()
        .map(|rule| rule.status_id)
        .collect();
    Ok(V1Row {
        keyword,
        filter,
        status_ids,
    })
}

/// Parent-side v1 update fields (all optional = unchanged).
#[derive(Debug, Clone, Default)]
pub struct V1Update {
    pub phrase: Option<String>,
    pub context: Option<Vec<String>>,
    /// `Some(None)` clears the expiry.
    pub expires_in_s: Option<Option<u64>>,
    pub irreversible: Option<bool>,
    pub whole_word: Option<bool>,
}

/// `PUT /api/v1/filters/:id` — update the keyword and (single-keyword
/// filters only) its parent attributes.
///
/// When the parent filter has MORE than one keyword and a parent attribute
/// (context/expires/irreversible) would change, answers the pinned 422.
pub fn v1_update(
    store: &dyn Store,
    viewer: Id,
    keyword_id: Id,
    update: V1Update,
    now_ms: u64,
) -> Result<V1Row> {
    let keyword = store
        .keyword_by_id(keyword_id)?
        .ok_or_else(not_found)?;
    let bundle = store
        .filter_by_id(keyword.filter_id)?
        .ok_or_else(not_found)?;
    if bundle.filter.account_id != viewer {
        return Err(not_found());
    }
    // Validate the supplied inputs (exact messages) before the guard.
    if let Some(context) = &update.context {
        validate_context(context)?;
    }
    if let Some(phrase) = &update.phrase {
        validate_keyword(phrase)?;
    }
    let new_action = update
        .irreversible
        .map(|irreversible| {
            if irreversible {
                FilterAction::Hide
            } else {
                FilterAction::Warn
            }
        })
        .filter(|action| *action != bundle.filter.action);
    let new_expiry = update
        .expires_in_s
        .map(|expires| expiry_ms(expires, now_ms))
        .filter(|expires| *expires != bundle.filter.expires_at_ms);
    let new_context = update
        .context
        .clone()
        .filter(|context| context != &bundle.filter.context);
    let parent_would_change =
        new_action.is_some() || new_expiry.is_some() || new_context.is_some();
    if bundle.keywords.len() > 1 && parent_would_change {
        return Err(Error::InvalidInput(ERR_V1_MULTI.into()));
    }
    // Keyword-level changes.
    if update.phrase.is_some() || update.whole_word.is_some() {
        store
            .update_keyword(
                viewer,
                keyword_id,
                update.phrase.as_deref().map(str::trim),
                update.whole_word,
                now_ms,
            )?
            .ok_or_else(not_found)?;
    }
    // Parent-level changes (only reachable for single-keyword filters).
    if parent_would_change {
        store
            .update_filter(
                viewer,
                bundle.filter.id,
                FilterPatch {
                    title: None,
                    context: new_context,
                    expires_at_ms: update
                        .expires_in_s
                        .map(|expires| expiry_ms(expires, now_ms)),
                    action: new_action,
                    keywords: None,
                },
                now_ms,
            )?
            .ok_or_else(not_found)?;
    }
    v1_show(store, viewer, keyword_id)
}

/// `DELETE /api/v1/filters/:id` — delete the keyword; the parent filter row
/// stays (Mastodon destroys only the keyword).
pub fn v1_destroy(store: &dyn Store, viewer: Id, keyword_id: Id) -> Result<()> {
    owned_keyword(store, viewer, keyword_id)?;
    store.delete_keyword(viewer, keyword_id)?.ok_or_else(not_found)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The `filtered`-array matcher.
// ---------------------------------------------------------------------------

/// Compile one keyword to a case-insensitive regex (`whole_word` wraps the
/// escaped keyword in `\b` boundaries).
fn compile_keyword(keyword: &FilterKeyword) -> Option<regex::Regex> {
    let escaped = regex::escape(keyword.keyword.trim());
    if escaped.is_empty() {
        return None;
    }
    let pattern = if keyword.whole_word {
        format!(r"\b(?:{escaped})\b")
    } else {
        escaped
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

/// The `filtered` array entries for one page of statuses.
///
/// Loads the viewer's filters for `context`, skips expired ones and matches
/// every keyword (regex over source text + CW + poll option titles + media
/// descriptions) and status rule (`status.id` or `status.reblog_of_id`).
/// Each match is a `REST::FilterResultSerializer` element keyed by status id;
/// reblog rows publish their entries under both their own id and the
/// reblogged status's id so wrappers can be filled by the caller.
///
/// Timelines/notifications are wired to this function by the integrator (one
/// call per page, after the page is loaded).
pub fn filter_results(
    store: &dyn Store,
    viewer: Id,
    statuses: &[Status],
    context: &str,
    now_ms: u64,
) -> Result<HashMap<Id, Vec<serde_json::Value>>> {
    filter_results_scoped(store, viewer, statuses, Some(context), now_ms)
}

/// Payload-shaped matching: EVERY active filter, regardless of `context`.
///
/// Mirrors Mastodon's `CustomFilter.apply_cached_filters`, which the status
/// relationships presenter uses for the `filtered` array — the `context`
/// field survives in the API for clients but does not narrow server-side
/// annotation (`app/presenters/status_relationships_presenter.rb` +
/// `custom_filter.rb:97` at the pinned tag).
pub fn filter_results_unscoped(
    store: &dyn Store,
    viewer: Id,
    statuses: &[Status],
    now_ms: u64,
) -> Result<HashMap<Id, Vec<serde_json::Value>>> {
    filter_results_scoped(store, viewer, statuses, None, now_ms)
}

fn filter_results_scoped(
    store: &dyn Store,
    viewer: Id,
    statuses: &[Status],
    context: Option<&str>,
    now_ms: u64,
) -> Result<HashMap<Id, Vec<serde_json::Value>>> {
    let mut out: HashMap<Id, Vec<serde_json::Value>> = HashMap::new();
    if statuses.is_empty() {
        return Ok(out);
    }
    let bundles: Vec<FilterBundle> = store
        .filters_by_account(viewer, context)?
        .into_iter()
        .filter(|bundle| {
            !bundle
                .filter
                .expires_at_ms
                .is_some_and(|expires| expires <= now_ms)
        })
        .collect();
    if bundles.is_empty() {
        return Ok(out);
    }
    // Compile each keyword once per page.
    let compiled: Vec<Vec<(String, regex::Regex)>> = bundles
        .iter()
        .map(|bundle| {
            bundle
                .keywords
                .iter()
                .filter_map(|keyword| {
                    compile_keyword(keyword).map(|re| (keyword.keyword.clone(), re))
                })
                .collect()
        })
        .collect();
    for status in statuses {
        let mut text = status.source_text.clone();
        if let Some(spoiler) = &status.spoiler_text {
            text.push('\n');
            text.push_str(spoiler);
        }
        if status.poll_id.is_some()
            && let Some(poll) = store.poll_by_status(status.id)?
        {
            for option in &poll.options {
                text.push('\n');
                text.push_str(&option.title);
            }
        }
        if !status.media_ids.is_empty() {
            let media = store.media_by_ids(&status.media_ids)?;
            for id in &status.media_ids {
                if let Some(attachment) = media.get(id) {
                    text.push('\n');
                    text.push_str(&attachment.description);
                }
            }
        }
        for (bundle, keywords) in bundles.iter().zip(&compiled) {
            let keyword_matches: Vec<String> = keywords
                .iter()
                .filter(|(_, re)| re.is_match(&text))
                .map(|(word, _)| word.clone())
                .collect();
            let status_matches: Vec<Id> = bundle
                .statuses
                .iter()
                .filter(|rule| {
                    rule.status_id == status.id || Some(rule.status_id) == status.reblog_of_id
                })
                .map(|rule| rule.status_id)
                .collect();
            if keyword_matches.is_empty() && status_matches.is_empty() {
                continue;
            }
            let entry = filter_result_json(bundle, keyword_matches, status_matches);
            out.entry(status.id).or_default().push(entry.clone());
            if let Some(reblog_of) = status.reblog_of_id {
                out.entry(reblog_of).or_default().push(entry);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Ownership helpers.
// ---------------------------------------------------------------------------

/// Resolve a filter the viewer owns (Mastodon 404 body for anything else).
fn owned_filter(store: &dyn Store, viewer: Id, id: Id) -> Result<FilterBundle> {
    let bundle = store.filter_by_id(id)?.ok_or_else(not_found)?;
    if bundle.filter.account_id != viewer {
        return Err(not_found());
    }
    Ok(bundle)
}

/// Resolve a keyword the viewer owns (ownership flows through the parent).
fn owned_keyword(store: &dyn Store, viewer: Id, id: Id) -> Result<(FilterKeyword, CustomFilter)> {
    let keyword = store.keyword_by_id(id)?.ok_or_else(not_found)?;
    let filter = store
        .filter_by_id(keyword.filter_id)?
        .map(|bundle| bundle.filter)
        .ok_or_else(not_found)?;
    if filter.account_id != viewer {
        return Err(not_found());
    }
    Ok((keyword, filter))
}
