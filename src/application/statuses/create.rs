//! Status creation orchestrator (`POST /api/v1/statuses`).
//!
//! Validation order follows Mastodon's `PostStatusService`: defaults and the
//! content-warning folding rule, media/visibility/language sanity, text
//! length/blank checks, reply resolution, mention/tag extraction, quote
//! resolution (visibility is finalized before the direct-mention rule), then
//! the poll.

use super::create_stages;
use crate::application::ports::Store;
use crate::domain::Id;
use crate::domain::Result;
use crate::domain::Status;

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
    let account_id = cmd.account_id;
    let (account, user) = create_stages::load_author(store, account_id)?;
    let (text, spoiler) = create_stages::fold_text(&cmd);
    create_stages::check_media_count(&cmd.media_ids)?;
    let (visibility, sensitive, language) =
        create_stages::resolve_defaults(&user, &account, &cmd, &spoiler)?;
    create_stages::check_text_shape(&text, &cmd, max_chars)?;
    let (in_reply_to, in_reply_to_account, mentions, tags) =
        create_stages::resolve_thread(store, account_id, &cmd, &text, base_url, now_ms)?;
    let (quote, visibility) =
        create_stages::resolve_quote(store, &cmd, &user, account_id, visibility, &mentions, now_ms)?;
    let poll = create_stages::assemble_poll(cmd.poll.clone(), now_ms)?;
    let place_id =
        create_stages::check_place_gate(store, place_policy, visibility, in_reply_to, place_id)?;
    create_stages::check_license_gate(store, license_policy, visibility, in_reply_to, &cmd, &account)?;
    create_stages::check_alt_text_gate(store, alt_text_policy, &cmd)?;
    let created = store.create_status(
        create_stages::build_new_status(
            &account,
            base_url,
            create_stages::StatusParts {
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
                media_ids: cmd.media_ids,
                place_id,
                poll,
                quote,
                application_name: cmd.application_name,
                application_website: cmd.application_website,
                idempotency_key: cmd.idempotency_key,
            },
        ),
        now_ms,
    )?;
    create_stages::journal_reply(
        store,
        created.replayed,
        in_reply_to,
        in_reply_to_account,
        account_id,
        now_ms,
    );
    Ok(CreateStatusResult {
        status: created.status,
        replayed: created.replayed,
    })
}
