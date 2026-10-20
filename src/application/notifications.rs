//! Notification fan-out (post-commit work) and the Phase 4 read surface.
//!
//! Notifications are created by the `FanOutNotification` job *after* the
//! interaction commits; the job is enqueued in the same transaction as the
//! interaction, so a committed interaction can never lose its notification,
//! and the notification store's dedup index makes job retries harmless.
//!
//! Mastodon v4.7.3 has no per-notification read flag: the only read state is
//! the per-user `notifications` marker, and `unread_count` counts rows with
//! `id > marker.last_read_id`. Index filtering (`types[]`/`exclude_types[]`)
//! narrows the rendered page but never the stored rows.

use serde_json::{Value, json};

use crate::application::ports::{NewNotification, Store};
use crate::domain::{Error, Id, Notification, NotificationType, Result};

/// Mastodon `DEFAULT_NOTIFICATIONS_LIMIT` / doubled maximum.
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 40;
/// Mastodon `DEFAULT_NOTIFICATIONS_COUNT_LIMIT` / maximum for `unread_count`.
pub const COUNT_DEFAULT_LIMIT: u64 = 100;
pub const COUNT_MAX_LIMIT: u64 = 1_000;

// ---------------------------------------------------------------------------
// Fan-out (Phase 3; unchanged).
// ---------------------------------------------------------------------------

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
/// Returns the id of a newly created notification (`None` on a dedup hit or a
/// skipped payload), so the caller can publish it once.
pub fn fan_out_from_payload(store: &dyn Store, payload: &Value) -> Result<Option<Id>> {
    let get_u64 = |key: &str| payload.get(key).and_then(|v| v.as_u64());
    let recipient_id = match get_u64("recipient_id") {
        Some(id) => Id::new(id),
        None => return Ok(None),
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
    let Some(kind) = kind else { return Ok(None) };
    let actor_id = get_u64("actor_id").map(Id::new);
    let status_id = get_u64("status_id").map(Id::new);
    let created_at_ms = get_u64("created_at_ms").unwrap_or_default();

    if actor_id == Some(recipient_id) {
        return Ok(None);
    }
    // D26 `NotifyService::DropCondition` parity: never create a notification
    // across a block (either direction), a mute, or a domain block. A domain
    // block still delivers when the recipient follows the actor, exactly as
    // upstream's `domain_blocking?` requires `not_following?`.
    if let Some(actor_id) = actor_id {
        if notification_drop(store, recipient_id, actor_id, created_at_ms)? {
            return Ok(None);
        }
    }
    let created = store.create_notification(NewNotification {
        recipient_id,
        r#type: kind,
        actor_id,
        status_id,
        created_at_ms,
    })?;
    Ok(created.map(|notification| notification.id))
}

/// D26: whether a notification from `actor` to `recipient` must be dropped at
/// creation (upstream `NotifyService::DropCondition`, minus the filter-policy,
/// conversation-mute and staff branches, which have no birchtree counterpart).
///
/// Drops when the recipient blocks the actor, is blocked by the actor, mutes
/// the actor (any active mute — deliberately broader than upstream, which
/// only honors notification-hiding mutes; see the D26 report note), or
/// domain-blocks the actor's domain while not following them. Missing actor
/// accounts fail closed (drop): a notification from an unknown account is
/// never deliverable.
///
/// Retroactive rule (operator question): pre-existing notifications are LEFT
/// in place when a block/mute/domain-block lands later. Rationale: stored
/// notifications are a historical record behind the `markers` read cursor
/// (`unread_count` counts `id > last_read_id`); upstream's `NotifyService`
/// has no retroactive deletion either, and open streams already retract via
/// `retraction_candidates`. No cleanup job is wired.
pub fn notification_drop(
    store: &dyn Store,
    recipient_id: Id,
    actor_id: Id,
    now_ms: u64,
) -> Result<bool> {
    let pair = store.pair_state(recipient_id, actor_id, now_ms)?;
    if pair.blocking || pair.blocked_by || pair.muting {
        return Ok(true);
    }
    let Some(actor) = store.account_by_id(actor_id)? else {
        return Ok(true);
    };
    let rows = store.relationships(recipient_id, &[(actor_id, actor.domain)], now_ms)?;
    if let Some(row) = rows.first() {
        if row.domain_blocking && !row.following {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Read surface (Phase 4).
// ---------------------------------------------------------------------------

/// Cursor-bounded query for `GET /api/v1/notifications`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
}

/// `GET /api/v1/notifications` — one page, newest first.
pub fn index(
    store: &dyn Store,
    recipient: Id,
    query: NotificationQuery,
) -> Result<Vec<Notification>> {
    store.notifications_page(
        recipient,
        query.limit,
        query.max_id,
        query.since_id,
        query.min_id,
    )
}

/// Keep only notifications whose type is in `types` (`types[]` semantics:
/// the intersection with the stored types; unknown values simply match
/// nothing).
pub fn retain_types(notifications: &mut Vec<Notification>, types: &[NotificationType]) {
    notifications.retain(|n| types.contains(&n.r#type));
}

/// `GET /api/v1/notifications/:id` — one notification of the recipient.
pub fn show(store: &dyn Store, recipient: Id, id: Id) -> Result<Notification> {
    store
        .notification_by_id(id)?
        .filter(|n| n.recipient_id == recipient)
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// `POST /api/v1/notifications/:id/dismiss` — delete one notification.
pub fn dismiss(store: &dyn Store, recipient: Id, id: Id) -> Result<()> {
    store
        .delete_notification(recipient, id)?
        .then_some(())
        .map(|_| ())
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// `POST /api/v1/notifications/clear` — delete every notification.
pub fn clear(store: &dyn Store, recipient: Id) -> Result<()> {
    store.clear_notifications(recipient)
}

/// `GET /api/v1/notifications/unread_count` — rows newer than the marker,
/// capped by `limit` (Mastodon `paginate_by_min_id(...).count`).
pub fn unread_count(store: &dyn Store, recipient: Id, limit: u64) -> Result<u64> {
    let marker = store
        .markers(recipient, &["notifications"])?
        .into_iter()
        .find(|m| m.timeline == "notifications");
    let floor = marker.map(|m| m.last_read_id);
    store.count_notifications_after(recipient, floor, limit)
}
