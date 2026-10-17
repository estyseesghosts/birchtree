//! Scheduled status use cases (Phase 4): scheduling via `POST
//! /api/v1/statuses` with `scheduled_at`, listing/updating/deleting, and the
//! durable publish job.
//!
//! Mirrors Mastodon's `PostStatusService#schedule_status!` (a 5-minute floor,
//! a 300 total / 25 per-day limit, and the past-time rule that silently
//! posts immediately) and `PublishScheduledStatusWorker` (destroy the row
//! first, then replay the stored params through the normal creation path).

use crate::application::ports::{NewScheduledStatus, Store};
use crate::application::statuses::{self, CreateStatusCommand, PollCommand};
use crate::domain::{Error, Id, Result, ScheduledStatus};

/// Mastodon en.yml `scheduled_statuses` messages (prefixed by Rails'
/// `Validation failed: `).
const ERR_TOO_SOON: &str = "Validation failed: Scheduled at date must be in the future";
const ERR_TOTAL: &str = "Validation failed: You have exceeded the limit of 300 scheduled posts";
const ERR_DAILY: &str =
    "Validation failed: You have exceeded the limit of 25 scheduled posts for today";
const ERR_INVALID_DATE: &str = "Record invalid";

/// `GET /api/v1/scheduled_statuses` — the account's scheduled statuses,
/// id-descending with cursors.
pub fn index(
    store: &dyn Store,
    account_id: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<ScheduledStatus>> {
    store.scheduled_by_account(account_id, max_id, since_id, min_id, limit)
}

/// `GET /api/v1/scheduled_statuses/:id`.
pub fn show(store: &dyn Store, account_id: Id, id: Id) -> Result<ScheduledStatus> {
    owned(store, account_id, id)
}

/// `PUT/PATCH /api/v1/scheduled_statuses/:id` — only `scheduled_at` is
/// editable.
pub fn reschedule(
    store: &dyn Store,
    account_id: Id,
    id: Id,
    scheduled_at_ms: u64,
    now_ms: u64,
) -> Result<ScheduledStatus> {
    owned(store, account_id, id)?;
    validate_time(scheduled_at_ms, now_ms)?;
    store
        .reschedule(account_id, id, scheduled_at_ms, now_ms)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// `DELETE /api/v1/scheduled_statuses/:id` — delete (publish job no-ops).
pub fn destroy(store: &dyn Store, account_id: Id, id: Id) -> Result<()> {
    owned(store, account_id, id)?;
    store.delete_scheduled(account_id, id)?;
    Ok(())
}

/// Schedule a validated status command for later publication.
///
/// The media check runs first (identical messages to immediate posts), then
/// the Mastodon validations: the 5-minute floor and the 300/25 limits.
pub fn schedule_status(
    store: &dyn Store,
    base_url: &str,
    now_ms: u64,
    cmd: CreateStatusCommand,
    scheduled_at_ms: u64,
) -> Result<ScheduledStatus> {
    let account_id = cmd.account_id;
    validate_time(scheduled_at_ms, now_ms)?;

    // Daily limit: rows whose scheduled time falls on the same UTC day. One
    // call returns (total, in-day) counts; Mastodon counts the row being
    // created against the current count.
    let day_start = scheduled_at_ms - scheduled_at_ms % 86_400_000;
    let (total, today) =
        store.scheduled_counts(account_id, day_start, day_start + 86_400_000)?;
    if total >= ScheduledStatus::TOTAL_LIMIT {
        return Err(Error::InvalidInput(ERR_TOTAL.into()));
    }
    if today >= ScheduledStatus::DAILY_LIMIT {
        return Err(Error::InvalidInput(ERR_DAILY.into()));
    }

    // Media validation (Mastodon `validate_media!` runs before scheduling).
    if !cmd.media_ids.is_empty() {
        let attachable = store.attachable_media(account_id, &cmd.media_ids, None)?;
        if attachable.len() != cmd.media_ids.len() {
            return Err(Error::InvalidInput(format!(
                "Media {} not found or already attached to another post",
                cmd.media_ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        for media in &attachable {
            if media.not_processed() {
                return Err(Error::InvalidInput(
                    "Cannot attach files that have not finished processing. Try again in a moment!"
                        .into(),
                ));
            }
        }
    }

    let params = command_params(&cmd);
    store.create_scheduled(
        NewScheduledStatus {
            account_id,
            scheduled_at_ms,
            media_ids: cmd.media_ids.clone(),
            params,
            base_url: base_url.to_string(),
        },
        now_ms,
    )
}

/// Execute a `PublishScheduledStatus` job: destroy the row and replay its
/// params through the normal status-creation use case.
///
/// Idempotent: a missing row means the publish already happened (or the
/// status was deleted). A row that was rescheduled into the future is left
/// alone; a fresh job is enqueued for its new due time.
pub fn publish_scheduled_job(
    store: &dyn Store,
    scheduled_id: Id,
    base_url: &str,
    now_ms: u64,
) -> Result<()> {
    let Some(row) = store.scheduled_by_id(scheduled_id)? else {
        return Ok(()); // Already published (or deleted): no-op.
    };
    if row.scheduled_at_ms > now_ms {
        // Rescheduled after this job was armed: re-arm under a fresh job id.
        store.enqueue_at(
            crate::domain::JobKind::PublishScheduledStatus,
            serde_json::json!({
                "scheduled_id": row.id.value(),
                "base_url": base_url,
            }),
            row.scheduled_at_ms,
            now_ms,
        )?;
        return Ok(());
    }
    // Mastodon destroys the row BEFORE publishing, so a crash mid-publish
    // never republishes; the media detach follows the same rule.
    store.delete_scheduled(row.account_id, row.id)?;

    let cmd = match command_from_params(row.account_id, &row.params) {
        Ok(cmd) => cmd,
        Err(err) => {
            tracing::warn!(scheduled = %row.id, error = %err, "scheduled publish params invalid");
            return Ok(());
        }
    };
    match statuses::create_status(store, base_url, now_ms, cmd) {
        Ok(_) => Ok(()),
        Err(err) => {
            // `PublishScheduledStatusWorker` swallows validation failures.
            tracing::warn!(scheduled = %row.id, error = %err, "scheduled publish dropped");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Params mapping.
// ---------------------------------------------------------------------------

/// Serialize a creation command into replayable params (Mastodon
/// `scheduled_options`).
fn command_params(cmd: &CreateStatusCommand) -> serde_json::Value {
    let mut params = serde_json::Map::new();
    if let Some(text) = &cmd.text {
        params.insert("text".into(), serde_json::json!(text));
    }
    if let Some(spoiler) = &cmd.spoiler_text {
        params.insert("spoiler_text".into(), serde_json::json!(spoiler));
    }
    params.insert("sensitive".into(), serde_json::json!(cmd.sensitive.unwrap_or(false)));
    if let Some(visibility) = &cmd.visibility {
        params.insert("visibility".into(), serde_json::json!(visibility));
    }
    if let Some(language) = &cmd.language {
        params.insert("language".into(), serde_json::json!(language));
    }
    if let Some(reply) = cmd.in_reply_to_id {
        params.insert("in_reply_to_id".into(), serde_json::json!(reply.value()));
    }
    if let Some(quote) = cmd.quoted_status_id {
        params.insert("quoted_status_id".into(), serde_json::json!(quote.value()));
    }
    if let Some(policy) = &cmd.quote_approval_policy {
        params.insert("quote_approval_policy".into(), serde_json::json!(policy));
    }
    params.insert(
        "media_ids".into(),
        serde_json::json!(cmd.media_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()),
    );
    if let Some(poll) = &cmd.poll {
        params.insert(
            "poll".into(),
            serde_json::json!({
                "multiple": poll.multiple,
                "hide_totals": poll.hide_totals,
                "expires_in": poll.expires_in_s,
                "options": poll.options,
            }),
        );
    }
    params.insert("scheduled_at".into(), serde_json::Value::Null);
    params.insert("idempotency".into(), serde_json::Value::Null);
    serde_json::Value::Object(params)
}

/// Rebuild a creation command from stored params (missing/unknown keys are
/// dropped, mirroring `options_with_objects` + `PostStatusService` defaults).
fn command_from_params(account_id: Id, params: &serde_json::Value) -> Result<CreateStatusCommand> {
    let get_str = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let in_reply_to_id = params
        .get("in_reply_to_id")
        .and_then(|v| v.as_u64())
        .map(Id::new);
    let quoted_status_id = params
        .get("quoted_status_id")
        .and_then(|v| v.as_u64())
        .map(Id::new);
    let media_ids = params
        .get("media_ids")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().and_then(|raw| raw.parse::<Id>().ok()))
                .collect()
        })
        .unwrap_or_default();
    let poll = params.get("poll").filter(|value| !value.is_null()).map(|value| {
        PollCommand {
            multiple: value.get("multiple").and_then(|v| v.as_bool()).unwrap_or(false),
            hide_totals: value.get("hide_totals").and_then(|v| v.as_bool()).unwrap_or(false),
            expires_in_s: value
                .get("expires_in")
                .and_then(|v| v.as_u64())
                .unwrap_or(86_400),
            options: value
                .get("options")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }
    });
    Ok(CreateStatusCommand {
        account_id,
        text: get_str("status").or_else(|| get_str("text")),
        in_reply_to_id,
        quoted_status_id,
        quote_approval_policy: get_str("quote_approval_policy"),
        sensitive: params.get("sensitive").and_then(|v| v.as_bool()),
        spoiler_text: get_str("spoiler_text"),
        visibility: get_str("visibility"),
        language: get_str("language"),
        media_ids,
        poll,
        application_name: None,
        application_website: None,
        idempotency_key: None,
    })
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Mastodon's 5-minute floor (`ScheduledStatus::MINIMUM_OFFSET`).
fn validate_time(scheduled_at_ms: u64, now_ms: u64) -> Result<()> {
    if scheduled_at_ms <= now_ms + ScheduledStatus::MINIMUM_OFFSET_MS {
        return Err(Error::InvalidInput(ERR_TOO_SOON.into()));
    }
    Ok(())
}

/// Owned scheduled row or the Mastodon 404.
fn owned(store: &dyn Store, account_id: Id, id: Id) -> Result<ScheduledStatus> {
    let row = store
        .scheduled_by_id(id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if row.account_id != account_id {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(row)
}

/// Parse a wire `scheduled_at` (RFC 3339 / ISO 8601). Returns `Ok(None)` for
/// a blank value (not scheduled), `Err` for an unparseable one (Mastodon
/// raises `RecordInvalid` → 422 `{"error":"Record invalid"}`).
pub fn parse_scheduled_at(raw: Option<&str>, now_ms: u64) -> Result<Option<u64>> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .or_else(|_| {
            chrono::DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f%:z")
        })
        .map(|dt| dt.timestamp_millis() as u64)
        .map_err(|_| Error::InvalidInput(ERR_INVALID_DATE.into()))?;
    if parsed <= now_ms {
        // Past (or now): Mastodon silently unschedules and posts immediately.
        return Ok(None);
    }
    Ok(Some(parsed))
}
