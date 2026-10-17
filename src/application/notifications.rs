//! Notification fan-out (post-commit work).
//!
//! Notifications are created by the `FanOutNotification` job *after* the
//! interaction commits; the job is enqueued in the same transaction as the
//! interaction, so a committed interaction can never lose its notification,
//! and the notification store's dedup index makes job retries harmless.

use serde_json::{Value, json};

use crate::application::ports::{NewNotification, Store};
use crate::domain::{Id, NotificationType, Result};

/// Build the payload for a `FanOutNotification` job.
pub fn fan_out_payload(
    recipient_id: Id,
    kind: NotificationType,
    actor_id: Option<Id>,
    status_id: Option<Id>,
    created_at_ms: u64,
) -> Value {
    json!({
        "recipient_id": recipient_id.value(),
        "type": kind.as_str(),
        "actor_id": actor_id.map(|id| id.value()),
        "status_id": status_id.map(|id| id.value()),
        "created_at_ms": created_at_ms,
    })
}

/// Execute a `FanOutNotification` job payload against the store.
///
/// Self-notifications are skipped; deduplication happens inside the store.
pub fn fan_out_from_payload(store: &dyn Store, payload: &Value) -> Result<()> {
    let get_u64 = |key: &str| payload.get(key).and_then(|v| v.as_u64());
    let recipient_id = match get_u64("recipient_id") {
        Some(id) => Id::new(id),
        None => return Ok(()),
    };
    let kind = payload
        .get("type")
        .and_then(|v| v.as_str())
        .and_then(|raw| match raw {
            "mention" => Some(NotificationType::Mention),
            "follow" => Some(NotificationType::Follow),
            "reblog" => Some(NotificationType::Reblog),
            "favourite" => Some(NotificationType::Favourite),
            "poll" => Some(NotificationType::PollVote),
            "follow_request" => Some(NotificationType::FollowRequest),
            "quote" => Some(NotificationType::Quote),
            _ => None,
        });
    let Some(kind) = kind else { return Ok(()) };
    let actor_id = get_u64("actor_id").map(Id::new);
    let status_id = get_u64("status_id").map(Id::new);
    let created_at_ms = get_u64("created_at_ms").unwrap_or_default();

    if actor_id == Some(recipient_id) {
        return Ok(());
    }
    store.create_notification(NewNotification {
        recipient_id,
        r#type: kind,
        actor_id,
        status_id,
        created_at_ms,
    })?;
    Ok(())
}
