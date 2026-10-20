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

use crate::application::ports::{NewNotification, Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{
    Error, Id, Notification, NotificationPolicy, NotificationRequest, NotificationType, PolicyAction,
    Result, Visibility,
};

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
        // Notification-policy pipeline (round 3, area C; upstream
        // `NotifyService` `FilterCondition`/`DropCondition` policy branches):
        // a filtered notification is stored hidden and filed on the request
        // queue instead of streaming; a dropped one leaves no trace.
        match evaluate_policy(store, recipient_id, kind, actor_id, status_id, created_at_ms)? {
            FilterVerdict::Accept => {}
            FilterVerdict::Drop => return Ok(None),
            FilterVerdict::Filter => {
                let created = store.create_notification(NewNotification {
                    recipient_id,
                    r#type: kind,
                    actor_id: Some(actor_id),
                    status_id,
                    created_at_ms,
                    filtered: true,
                })?;
                // Request rows exist only for mention/quote notifications
                // (upstream `update_notification_request!` returns for other
                // types, which stay filtered with no row).
                if created.is_some()
                    && matches!(
                        kind,
                        NotificationType::Mention | NotificationType::Quote
                    )
                {
                    if let Some(status_id) = status_id {
                        store.upsert_notification_request(
                            recipient_id,
                            actor_id,
                            status_id,
                            created_at_ms,
                        )?;
                    }
                }
                // No `notification` event and no push: upstream pushes
                // nothing for a filtered notification.
                return Ok(None);
            }
        }
    }
    let created = store.create_notification(NewNotification {
        recipient_id,
        r#type: kind,
        actor_id,
        status_id,
        created_at_ms,
        filtered: false,
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

// ---------------------------------------------------------------------------
// Notification-policy filter pipeline (round 3, area C).
// ---------------------------------------------------------------------------
//
// Upstream reference (Mastodon v4.7.3, `8ff11c2b`):
// `app/services/notify_service.rb` (`DropCondition`, `FilterCondition`,
// `update_notification_request!`), `app/models/notification_policy.rb`
// (column defaults), `app/models/notification.rb` (`PROPERTIES`
// `filterable`), `app/services/accept_notification_request_service.rb`,
// `app/services/dismiss_notification_request_service.rb`,
// `app/workers/unfilter_notifications_worker.rb:42` (the
// `notifications_merged` publish).

/// Outcome of the policy evaluation for one notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterVerdict {
    /// Deliver normally (`notification` event + push).
    Accept,
    /// Store hidden (`filtered`) and file/update the request row.
    Filter,
    /// Leave no trace (policy `drop`, like the D26 base drops).
    Drop,
}

/// Upstream `NEW_FOLLOWER_THRESHOLD` (3 days) and `NEW_ACCOUNT_THRESHOLD`
/// (30 days), in milliseconds.
pub const NEW_FOLLOWER_THRESHOLD_MS: u64 = 3 * 24 * 60 * 60 * 1000;
pub const NEW_ACCOUNT_THRESHOLD_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Whether a notification of this type can be filtered (upstream
/// `Notification::PROPERTIES[*][:filterable]`; only the types this server
/// produces are listed: mention, reblog, follow, follow_request, favourite
/// and quote are filterable, poll is not).
pub fn filterable_type(kind: NotificationType) -> bool {
    !matches!(kind, NotificationType::PollVote)
}

/// Evaluate the recipient's notification policy for one notification
/// (upstream `FilterCondition#filter?` / `DropCondition` policy branches).
///
/// `status_id` is needed for the private-mention check; notifications without
/// an actor never reach here (the caller files those unfiltered).
pub fn evaluate_policy(
    store: &dyn Store,
    recipient_id: Id,
    kind: NotificationType,
    actor_id: Id,
    status_id: Option<Id>,
    now_ms: u64,
) -> Result<FilterVerdict> {
    if !filterable_type(kind) {
        return Ok(FilterVerdict::Accept);
    }
    // Accepted actors bypass the filter forever (upstream
    // `FilterCondition#override_for_sender?` via `NotificationPermission`).
    if store.has_notification_permission(recipient_id, actor_id)? {
        return Ok(FilterVerdict::Accept);
    }
    let policy = store.notification_policy(recipient_id)?;
    let Some(sender) = store.account_by_id(actor_id)? else {
        // Missing actor rows fail closed (drop), like `notification_drop`.
        return Ok(FilterVerdict::Drop);
    };
    let pair = store.pair_state(recipient_id, actor_id, now_ms)?;
    let not_following = !pair.following;
    let not_follower = match store.follow_of(actor_id, recipient_id)? {
        None => true,
        // Upstream `not_follower?`: missing, younger than
        // `NEW_FOLLOWER_THRESHOLD` (3 days), or not an active follow.
        Some(follow) => {
            follow.created_at_ms.saturating_add(NEW_FOLLOWER_THRESHOLD_MS) > now_ms
                || follow.state != crate::domain::FollowState::Active
        }
    };
    let new_account = sender.created_at_ms.saturating_add(NEW_ACCOUNT_THRESHOLD_MS) > now_ms;
    let private_mention = is_private_mention(store, kind, actor_id, status_id)?;
    let limited = sender.silenced;
    // No bot marker exists on the account row, so `for_bots` never matches;
    // the flag is stored and rendered honestly, and matches nothing until a
    // bot marker lands (recorded in the D20 register note).
    let bot = false;

    let drop = (policy.for_not_following == PolicyAction::Drop && not_following)
        || (policy.for_not_followers == PolicyAction::Drop && not_follower)
        || (policy.for_new_accounts == PolicyAction::Drop && new_account && not_following)
        || (policy.for_private_mentions == PolicyAction::Drop
            && not_following
            && private_mention)
        || (policy.for_limited_accounts == PolicyAction::Drop && limited && not_following)
        || (policy.for_bots == PolicyAction::Drop && bot && not_following);
    if drop {
        return Ok(FilterVerdict::Drop);
    }
    let filter = (policy.for_not_following == PolicyAction::Filter && not_following)
        || (policy.for_not_followers == PolicyAction::Filter && not_follower)
        || (policy.for_new_accounts == PolicyAction::Filter && new_account && not_following)
        || (policy.for_private_mentions == PolicyAction::Filter
            && not_following
            && private_mention)
        || (policy.for_limited_accounts == PolicyAction::Filter && limited && not_following)
        || (policy.for_bots == PolicyAction::Filter && bot && not_following);
    Ok(if filter {
        FilterVerdict::Filter
    } else {
        FilterVerdict::Accept
    })
}

/// Upstream `FilterCondition#private_mention_not_in_response?`: a mention
/// whose status is direct-visibility and is not a reply whose parent
/// mentions the sender. (Upstream counts any thread status mentioning the
/// sender; this server checks the direct parent's mention list — the common
/// case — and records the simplification in the D20 register note.)
fn is_private_mention(
    store: &dyn Store,
    kind: NotificationType,
    actor_id: Id,
    status_id: Option<Id>,
) -> Result<bool> {
    if kind != NotificationType::Mention {
        return Ok(false);
    }
    let Some(status_id) = status_id else {
        return Ok(false);
    };
    let Some(status) = store.status_by_id(status_id)? else {
        return Ok(false);
    };
    if status.visibility != Visibility::Direct {
        return Ok(false);
    }
    let Some(parent_id) = status.in_reply_to_id else {
        return Ok(true);
    };
    let Ok(Some(parent)) = store.status_by_id(parent_id) else {
        return Ok(true);
    };
    Ok(!parent.mentions.contains(&actor_id))
}

/// `GET /api/v1/notifications/policy` — the recipient's policy row
/// (upstream defaults when never stored).
pub fn policy(store: &dyn Store, account_id: Id) -> Result<NotificationPolicy> {
    store.notification_policy(account_id)
}

/// One `filter_*` boolean from `PUT /api/v1/notifications/policy` (upstream
/// `PoliciesController#resource_params` + the V1 compat setters:
/// `true` → `filter`, `false` → `accept`). `for_limited_accounts` is not
/// settable through this surface, exactly like upstream's V1 params.
#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyChanges {
    pub filter_not_following: Option<bool>,
    pub filter_not_followers: Option<bool>,
    pub filter_new_accounts: Option<bool>,
    pub filter_private_mentions: Option<bool>,
    pub filter_bots: Option<bool>,
}

impl PolicyChanges {
    pub fn is_empty(self) -> bool {
        self.filter_not_following.is_none()
            && self.filter_not_followers.is_none()
            && self.filter_new_accounts.is_none()
            && self.filter_private_mentions.is_none()
            && self.filter_bots.is_none()
    }
}

/// `PUT /api/v1/notifications/policy` — apply the given flags to the
/// recipient's row (absent flags keep their stored value).
pub fn update_policy(
    store: &dyn Store,
    account_id: Id,
    changes: PolicyChanges,
) -> Result<NotificationPolicy> {
    let mut policy = store.notification_policy(account_id)?;
    let apply = |flag: Option<bool>| -> Option<PolicyAction> {
        flag.map(|filter| {
            if filter {
                PolicyAction::Filter
            } else {
                PolicyAction::Accept
            }
        })
    };
    if let Some(action) = apply(changes.filter_not_following) {
        policy.for_not_following = action;
    }
    if let Some(action) = apply(changes.filter_not_followers) {
        policy.for_not_followers = action;
    }
    if let Some(action) = apply(changes.filter_new_accounts) {
        policy.for_new_accounts = action;
    }
    if let Some(action) = apply(changes.filter_private_mentions) {
        policy.for_private_mentions = action;
    }
    if let Some(action) = apply(changes.filter_bots) {
        policy.for_bots = action;
    }
    store.set_notification_policy(&policy)
}

/// `GET /api/v1/notifications/requests` rows, newest first (the endpoint
/// applies the `max_id`/`since_id`/`min_id` cursors + `Link`, like the
/// notifications index).
pub fn requests(store: &dyn Store, recipient: Id) -> Result<Vec<NotificationRequest>> {
    store.notification_requests(recipient)
}

/// `GET /api/v1/notifications/requests/:id`.
pub fn request_show(
    store: &dyn Store,
    recipient: Id,
    id: Id,
) -> Result<NotificationRequest> {
    store
        .notification_request(recipient, id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// Outcome of an accept: whether anything actually merged (the
/// `notifications_merged` event fires only on a real merge — this is what
/// the D20 lock test pins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptOutcome {
    pub unfiltered: u64,
    pub merged_event_sent: bool,
}

/// `POST /api/v1/notifications/requests/:id/accept` (upstream
/// `AcceptNotificationRequestService`): grant the bypass permission, unhide
/// the actor's filtered rows, destroy the request, and publish
/// `notifications_merged` — but only when rows actually merged. Accept runs
/// synchronously (no worker counter), so `merged?` is simply "no pending
/// requests".
pub fn accept_request(
    store: &dyn Store,
    streaming: &dyn StreamingSink,
    recipient: Id,
    id: Id,
) -> Result<AcceptOutcome> {
    let request = request_show(store, recipient, id)?;
    store.grant_notification_permission(recipient, request.from_account_id)?;
    let unfiltered = store.unfilter_notifications(recipient, request.from_account_id)?;
    store.delete_notification_request(recipient, id)?;
    let merged_event_sent = if unfiltered > 0 {
        streaming.publish(StreamEvent::NotificationsMerged {
            recipient_id: recipient,
        });
        true
    } else {
        false
    };
    Ok(AcceptOutcome {
        unfiltered,
        merged_event_sent,
    })
}

/// `POST /api/v1/notifications/requests/:id/dismiss` (upstream
/// `DismissNotificationRequestService` + `FilteredNotificationCleanupWorker`,
/// run synchronously): delete the actor's filtered rows and destroy the
/// request. No streaming event — a dismissal is silent, like upstream.
pub fn dismiss_request(store: &dyn Store, recipient: Id, id: Id) -> Result<u64> {
    let request = request_show(store, recipient, id)?;
    let removed = store.delete_filtered_notifications(recipient, request.from_account_id)?;
    store.delete_notification_request(recipient, id)?;
    Ok(removed)
}

/// Bulk accept/dismiss (`POST .../requests/accept|dismiss`, upstream
/// `accept_bulk`/`dismiss_bulk`): each id is handled independently;
/// unknown or foreign ids are skipped (upstream scopes the set to the
/// recipient's rows, so they simply match nothing).
pub fn accept_requests_bulk(
    store: &dyn Store,
    streaming: &dyn StreamingSink,
    recipient: Id,
    ids: &[Id],
) -> Result<AcceptOutcome> {
    let mut unfiltered = 0u64;
    let mut merged_event_sent = false;
    for id in ids {
        if let Ok(outcome) = accept_request(store, streaming, recipient, *id) {
            unfiltered += outcome.unfiltered;
            merged_event_sent |= outcome.merged_event_sent;
        }
    }
    Ok(AcceptOutcome {
        unfiltered,
        merged_event_sent,
    })
}

/// Bulk dismiss: returns the total filtered rows removed.
pub fn dismiss_requests_bulk(
    store: &dyn Store,
    recipient: Id,
    ids: &[Id],
) -> Result<u64> {
    let mut removed = 0u64;
    for id in ids {
        if let Ok(count) = dismiss_request(store, recipient, *id) {
            removed += count;
        }
    }
    Ok(removed)
}

/// `GET /api/v1/notifications/requests/merged` (upstream `requests#merged?`):
/// synchronously-processed accepts leave no worker counter behind, so
/// "merged" is exactly "no pending requests".
pub fn merged(store: &dyn Store, recipient: Id) -> Result<bool> {
    Ok(store.notification_requests(recipient)?.is_empty())
}
