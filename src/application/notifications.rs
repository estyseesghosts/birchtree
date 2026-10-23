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
use crate::application::push::PushSubscriptionStore;
use crate::application::streaming::StreamEvent;
use crate::domain::{
    Error, Id, Notification, NotificationPolicy, NotificationRequest, NotificationType, PolicyAction,
    Result, Visibility, WebPushSubscription,
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
        .and_then(NotificationType::parse);
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
        match evaluate_policy(
            &NotificationCtx {
                store,
                recipient_id,
                now_ms: created_at_ms,
            },
            kind,
            actor_id,
            status_id,
        )? {
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
    /// Only notifications from this actor (`browserable_params[:account_id]`).
    /// `None` = no actor filter.
    pub from_account: Option<Id>,
    /// Accepted for upstream param parity (`browserable_params` permits
    /// `include_filtered`): when true the caller is expected to ALSO fetch
    /// the recipient's filtered rows (via the `filtered_notifications_page`
    /// store listing — parent snippet on `ports.rs` + `redb`) and merge them
    /// with [`merge_filtered_index`]. The store page below still excludes
    /// filtered rows (upstream's default notification scope), so an
    /// unsupplemented `index` call with this flag behaves like `false` —
    /// never fabricated rows. DEFERRED (not a gap): pushing
    /// `from_account`/`include_filtered` into the store page is a bounded
    /// perf optimization only — the `index` fill loop below is correct
    /// (cursor-stable, full pages) with the current store shape, and a naive
    /// in-page filter would break the loop's end-of-scan detection. Revisit
    /// only with measured scan pressure.
    pub include_filtered: bool,
}

/// `GET /api/v1/notifications` — one page, newest first.
///
/// Upstream `without_suspended` scope: notifications whose actor is suspended
/// are excluded (and `show` answers 404 for them). The store page has no
/// suspension or actor filter, so this loops bounded page fetches (at most a
/// few rounds: exclusions are rare) and advances `max_id` past excluded rows,
/// keeping pages full and cursors stable.
pub fn index(
    store: &dyn Store,
    recipient: Id,
    query: NotificationQuery,
) -> Result<Vec<Notification>> {
    let mut out: Vec<Notification> = Vec::new();
    let mut max_id = query.max_id;
    // Bounded fill loop: each round fetches one page past the excluded rows.
    for _ in 0..8 {
        let remaining = query.limit.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        let page = store.notifications_page(
            recipient,
            remaining,
            max_id,
            query.since_id,
            query.min_id,
        )?;
        if page.is_empty() {
            break;
        }
        let mut excluded = 0usize;
        for notification in page {
            max_id = Some(notification.id);
            if query
                .from_account
                .is_some_and(|actor| notification.actor_id != Some(actor))
            {
                excluded += 1;
                continue;
            }
            if notification_excluded(store, &notification)? {
                excluded += 1;
                continue;
            }
            out.push(notification);
            if out.len() >= query.limit {
                break;
            }
        }
        // A short or fully-excluded page ends the scan (a full page of
        // exclusions retries past them; the round cap bounds the scan).
        if excluded == 0 {
            break;
        }
    }
    out.truncate(query.limit);
    Ok(out)
}

/// Whether a notification is hidden from the browserable surface: upstream's
/// `without_suspended` join drops rows whose actor is suspended (or gone —
/// the inner join drops actor-less rows too).
///
/// Actor-less rows (`moderation_warning`, `annual_report`,
/// `severed_relationships`, `admin.*`) are VISIBLE here: upstream maps those
/// to the recipient's own account (`notification.rb#set_from_account`), and
/// the REST render falls back to the recipient's account for them (endpoint
/// snippet). The pre-existing `None => true` rule would hide exactly the
/// D51 rows this module now produces, so it was narrowed to suspended/gone
/// actors only.
fn notification_excluded(store: &dyn Store, notification: &Notification) -> Result<bool> {
    let Some(actor_id) = notification.actor_id else {
        return Ok(false);
    };
    match store.account_by_id(actor_id)? {
        Some(actor) => Ok(actor.suspended),
        None => Ok(true),
    }
}

/// Keep only notifications whose type is in `types` (`types[]` semantics:
/// the intersection with the stored types; unknown values simply match
/// nothing).
pub fn retain_types(notifications: &mut Vec<Notification>, types: &[NotificationType]) {
    notifications.retain(|n| types.contains(&n.r#type));
}

/// `GET /api/v1/notifications/:id` — one notification of the recipient.
///
/// Upstream scopes the find with `without_suspended`: a notification whose
/// actor is suspended (or gone) answers 404, like the index exclusion.
pub fn show(store: &dyn Store, recipient: Id, id: Id) -> Result<Notification> {
    let notification = store
        .notification_by_id(id)?
        .filter(|n| n.recipient_id == recipient)
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if notification_excluded(store, &notification)? {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(notification)
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
/// `Notification::PROPERTIES[*][:filterable]`: mention, reblog, follow,
/// follow_request, favourite, quote and added_to_collection are filterable;
/// every other type is not).
pub fn filterable_type(kind: NotificationType) -> bool {
    matches!(
        kind,
        NotificationType::Mention
            | NotificationType::Reblog
            | NotificationType::Follow
            | NotificationType::FollowRequest
            | NotificationType::Favourite
            | NotificationType::Quote
            | NotificationType::AddedToCollection
    )
}

/// Ambient read context shared by the notification-policy evaluation and the
/// REST/stream rendering of notification rows.
///
/// Bundles only the handles that are constant across a request (or a fan-out
/// job): the store, the recipient, and the clock reading. Per-notification
/// inputs (type, actor, status) and per-request wire inputs (`ServerInfo`,
/// `supported_types[]`) stay explicit parameters at each call site so the
/// application layer never imports adapter types.
#[derive(Clone, Copy)]
pub struct NotificationCtx<'a> {
    /// Store handle for policy rows, accounts, and statuses.
    pub store: &'a dyn Store,
    /// Owner of the notification rows being evaluated or rendered.
    pub recipient_id: Id,
    /// Clock reading (`state.context.now_ms()` / job `created_at_ms` basis).
    pub now_ms: u64,
}

/// Evaluate the recipient's notification policy for one notification
/// (upstream `FilterCondition#filter?` / `DropCondition` policy branches).
///
/// `status_id` is needed for the private-mention check; notifications without
/// an actor never reach here (the caller files those unfiltered).
pub fn evaluate_policy(
    ctx: &NotificationCtx<'_>,
    kind: NotificationType,
    actor_id: Id,
    status_id: Option<Id>,
) -> Result<FilterVerdict> {
    let store = ctx.store;
    let recipient_id = ctx.recipient_id;
    let now_ms = ctx.now_ms;
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

// ---------------------------------------------------------------------------
// D51 producers + D52 read-path follow-ups (gaps batch, G-B).
// ---------------------------------------------------------------------------
//
// Upstream reference: `app/models/notification.rb` (`PROPERTIES`, `TYPES`,
// `set_from_account`, `browserable`), `app/services/notify_service.rb`,
// `app/services/fan_out_on_write_service.rb`,
// `app/serializers/concerns/notification_fallback_concern.rb` (all at the
// v4.7.3 pin `8ff11c2b`, verified against upstream `main` where the pin
// source was unavailable).
//
// Shape of every producer below: a PURE builder returning
// `(JobKind::FanOutNotification, payload)` pairs. The triggering mutation's
// transaction inserts each pair with `super::jobs::insert_job` in the SAME
// transaction (`birchtree.md` §5: mutation + indexes + counters + jobs
// commit atomically; fan-out runs after commit via the `FanOutNotification`
// handler). The builder never touches storage, so it cannot observe a
// half-committed state; recipient selection happens at the call site from
// data the transaction already holds (or from bounded pre-existing-row
// reads, which are unaffected by the open write). Call-site insertions live
// in files owned by others — each builder names its file + anchor and the
// parent merges the verbatim snippet from the final report.
//
// A builder given an empty recipient list returns `[]`: no fabricated rows.
// Types whose trigger feature does not exist yet (`severed_relationships`
// — domain blocks sever nothing; `added_to_collection`/`collection_update`
// — no collections surface) have real builders but no call site, so the
// observable outcome matches upstream with no such activity (verified by
// `tests/gaps_notifications.rs`, which asserts empty pages for them).

use crate::domain::JobKind;

/// One durable fan-out job to insert in the triggering transaction.
#[derive(Debug, Clone)]
pub struct PendingFanOut {
    pub kind: JobKind,
    pub payload: serde_json::Value,
}

fn pending(
    recipient_id: Id,
    kind: NotificationType,
    actor_id: Option<Id>,
    status_id: Option<Id>,
    now_ms: u64,
) -> PendingFanOut {
    PendingFanOut {
        kind: JobKind::FanOutNotification,
        payload: fan_out_payload(recipient_id, kind, actor_id, status_id, now_ms),
    }
}

fn pending_for_many(
    recipients: &[Id],
    kind: NotificationType,
    actor_id: Option<Id>,
    status_id: Option<Id>,
    now_ms: u64,
) -> Vec<PendingFanOut> {
    recipients
        .iter()
        .map(|recipient| pending(*recipient, kind, actor_id, status_id, now_ms))
        .collect()
}

/// `status` — "someone whose posts you asked to be notified about posted"
/// (upstream: followers with `Follow.notify`, fanned out on write).
/// Call site: `persistence/redb/statuses.rs::create_status`, after the
/// mentions block, before `write.commit()`. `notify_followers` = author's
/// followers with `follow.notify` set (bounded scan at the snippet).
pub fn emit_status_for_followers(
    author_id: Id,
    status_id: Id,
    notify_followers: &[Id],
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        notify_followers,
        NotificationType::Status,
        Some(author_id),
        Some(status_id),
        now_ms,
    )
}

/// `update` — "a status you boosted was edited" (upstream
/// `docs.joinmastodon.org/entities/Notification`). Recipients = accounts
/// holding a live reblog wrapper of the edited status.
/// Call site: `persistence/redb/statuses.rs::update_status`, before the
/// `:861` commit. Recipients via `self.reblogged_by(id, None, cap)`.
pub fn emit_update_for_rebloggers(
    editor_id: Id,
    status_id: Id,
    reblogger_ids: &[Id],
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        reblogger_ids,
        NotificationType::Update,
        Some(editor_id),
        Some(status_id),
        now_ms,
    )
}

/// `quoted_update` — "a status you quoted was edited". Recipients = authors
/// of accepted quotes of the edited status; actor = the quoted author.
/// Call site: `persistence/redb/statuses.rs::update_status`, before the
/// `:861` commit. Recipients via `self.quotes_of_status(id, None, cap)`
/// (accepted states only, authors of the quoting statuses).
pub fn emit_quoted_update_for_quoters(
    quoted_author_id: Id,
    quoted_status_id: Id,
    quoter_ids: &[Id],
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        quoter_ids,
        NotificationType::QuotedUpdate,
        Some(quoted_author_id),
        Some(quoted_status_id),
        now_ms,
    )
}

/// `moderation_warning` — "a moderator acted on your account". Actor-less:
/// upstream `set_from_account` maps warning/severance/annual-report rows to
/// the recipient's own account, and the REST render falls back to it.
/// Call site: `persistence/redb/admin.rs::admin_account_action`, after the
/// `ACCOUNT_WARNINGS` insert, before `write.commit()`.
pub fn emit_moderation_warning(target_account_id: Id, now_ms: u64) -> Vec<PendingFanOut> {
    vec![pending(
        target_account_id,
        NotificationType::ModerationWarning,
        None,
        None,
        now_ms,
    )]
}

/// `annual_report` — "your yearly report is ready". Actor-less (see
/// [`emit_moderation_warning`]).
/// Call site: `persistence/redb/trends.rs::insert_annual_report`, before
/// `write.commit()`.
pub fn emit_annual_report(account_id: Id, now_ms: u64) -> Vec<PendingFanOut> {
    vec![pending(
        account_id,
        NotificationType::AnnualReport,
        None,
        None,
        now_ms,
    )]
}

/// `admin.sign_up` — "someone signed up", to every admin. Actor = the new
/// account.
/// Call site: account creation (`application/accounts.rs` registration path
/// + `create_local_account` store txn; G-E owned). Recipients via
/// `admin_accounts_page` with `staff_only` filtered to role `admin`.
pub fn emit_admin_sign_up(
    new_account_id: Id,
    admin_ids: &[Id],
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        admin_ids,
        NotificationType::AdminSignUp,
        Some(new_account_id),
        None,
        now_ms,
    )
}

/// `admin.report` — "a new report was filed", to staff (admins +
/// moderators). Actor = the reporter.
/// Call site: `persistence/redb/federation_content.rs::insert_report`,
/// before `write.commit()`. Recipients via `admin_accounts_page` with
/// `staff_only` (both roles; upstream notifies reviewers, which here is the
/// whole staff set).
pub fn emit_admin_report(
    reporter_id: Id,
    staff_ids: &[Id],
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        staff_ids,
        NotificationType::AdminReport,
        Some(reporter_id),
        None,
        now_ms,
    )
}

/// `severed_relationships` — "follow relationships were severed by a
/// moderation/block event". Actor-less (see [`emit_moderation_warning`]).
/// UNWIRED: `domain_block` severs nothing (no follow cascade exists), and
/// blocks sever endorsements only — so there is no trigger transaction yet.
/// The builder is real; the test pins the empty observable outcome until a
/// sever cascade lands.
pub fn emit_severed_relationships(recipient_id: Id, now_ms: u64) -> Vec<PendingFanOut> {
    vec![pending(
        recipient_id,
        NotificationType::SeveredRelationships,
        None,
        None,
        now_ms,
    )]
}

/// `added_to_collection` — "someone added you to a collection".
/// Actor = the collection owner.
/// UNWIRED: no collections surface yet (G-C). Real builder, no call site.
pub fn emit_added_to_collection(
    member_id: Id,
    owner_id: Id,
    now_ms: u64,
) -> Vec<PendingFanOut> {
    vec![pending(
        member_id,
        NotificationType::AddedToCollection,
        Some(owner_id),
        None,
        now_ms,
    )]
}

/// `collection_update` — "a collection you are in was updated".
/// Actor = the collection owner.
/// UNWIRED: no collections surface yet (G-C). Real builder, no call site.
pub fn emit_collection_update(
    member_ids: &[Id],
    owner_id: Id,
    now_ms: u64,
) -> Vec<PendingFanOut> {
    pending_for_many(
        member_ids,
        NotificationType::CollectionUpdate,
        Some(owner_id),
        None,
        now_ms,
    )
}

// ---------------------------------------------------------------------------
// `include_filtered` merge (D52-adjacent read path; G-B application half).
// ---------------------------------------------------------------------------

/// Merge a store page (unfiltered rows, newest first) with a filtered-rows
/// listing for `include_filtered=true` (upstream `browserable` serves
/// filtered rows when `include_filtered` is set — or whenever `account_id`
/// narrows the page).
///
/// Both inputs are expected pre-windowed by the caller; this re-applies the
/// query's actor filter and exclusive numeric cursors defensively, merges
/// newest-first by NUMERIC id (decimal-string ids never compare
/// lexicographically), and truncates to `limit`. Pure: fully testable
/// without storage.
pub fn merge_filtered_index(
    page: Vec<Notification>,
    filtered: Vec<Notification>,
    query: &NotificationQuery,
) -> Vec<Notification> {
    let mut out: Vec<Notification> = Vec::with_capacity(page.len() + filtered.len());
    for notification in page.into_iter().chain(filtered) {
        if query
            .from_account
            .is_some_and(|actor| notification.actor_id != Some(actor))
        {
            continue;
        }
        if let Some(max_id) = query.max_id {
            if notification.id.value() >= max_id.value() {
                continue;
            }
        }
        if let Some(since_id) = query.since_id {
            if notification.id.value() <= since_id.value() {
                continue;
            }
        }
        if let Some(min_id) = query.min_id {
            if notification.id.value() <= min_id.value() {
                continue;
            }
        }
        out.push(notification);
    }
    out.sort_by(|a, b| b.id.value().cmp(&a.id.value()));
    out.truncate(query.limit);
    out
}

// ---------------------------------------------------------------------------
// `supported_types` fallback rendering (D52-adjacent read path).
// ---------------------------------------------------------------------------

/// Upstream `Notification::PROPERTIES[*][:baseline]` (verified against the
/// pin): baseline clients render these types natively, so they never carry
/// a `fallback`. Every other type is a fallback candidate.
pub fn baseline_type(kind: NotificationType) -> bool {
    !matches!(
        kind,
        NotificationType::SeveredRelationships
            | NotificationType::ModerationWarning
            | NotificationType::AdminSignUp
            | NotificationType::AdminReport
            | NotificationType::AddedToCollection
            | NotificationType::CollectionUpdate
    )
}

/// Whether the v1 index/show must attach a `fallback` object to this
/// notification (upstream `NotificationFallbackConcern#needs_fallback?`):
/// - `None` (`supported_types[]` absent) → never: the parameter "is required
///   to get any notification fallback at all".
/// - baseline types, or types the client listed → never.
/// - `severed_relationships` needs its severance event, `admin.report` its
///   report, collection types their collection (`activity_present`); without
///   the activity "we can't do much" → no fallback (never fabricated).
/// - everything else non-baseline → fallback.
pub fn needs_fallback(
    kind: NotificationType,
    supported: Option<&[NotificationType]>,
    activity_present: bool,
) -> bool {
    let Some(supported) = supported else {
        return false;
    };
    if baseline_type(kind) || supported.contains(&kind) {
        return false;
    }
    match kind {
        NotificationType::SeveredRelationships
        | NotificationType::AdminReport
        | NotificationType::AddedToCollection
        | NotificationType::CollectionUpdate => activity_present,
        _ => true,
    }
}

/// Build the `fallback` object for a non-baseline notification (upstream
/// `NotificationFallbackConcern#fallback`: `{title, summary, description}` —
/// note the third key is `description`, not the docs-site `details`; the
/// concern source wins).
///
/// Titles mirror the upstream locale strings with the actor's display name
/// (`@user`) substituted for the mention links (no HTML linkification
/// server-side here — plain text, never fabricated links). Summaries point
/// at signing in to review, like upstream's generic summary. `description`
/// is always null, exactly like upstream (`description: nil`).
pub fn fallback_for(kind: NotificationType, actor_name: &str) -> serde_json::Value {
    const SIGN_IN_SUMMARY: &str = "Sign in to review this notification in full.";
    let title = match kind {
        NotificationType::SeveredRelationships => {
            "Some of your follow relationships were severed".to_string()
        }
        NotificationType::ModerationWarning => {
            "A moderator has taken action on your account".to_string()
        }
        NotificationType::AdminSignUp => format!("@{actor_name} signed up"),
        NotificationType::AdminReport => format!("@{actor_name} filed a new report"),
        NotificationType::AddedToCollection => format!("@{actor_name} added you to a collection"),
        NotificationType::CollectionUpdate => {
            "A collection you are featured in was updated".to_string()
        }
        _ => format!("New notification from @{actor_name}"),
    };
    json!({
        "title": title,
        "summary": SIGN_IN_SUMMARY,
        "description": null,
    })
}

// ---------------------------------------------------------------------------
// Pre-token push backfill (D52-adjacent; G-B application half).
// ---------------------------------------------------------------------------

/// Outcome of [`backfill_push_unsubscribe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushBackfillOutcome {
    /// No subscription for this token: nothing to heal (never created).
    Missing,
    /// The row already carries an unsubscribe URL: returned untouched (no
    /// write, so `updated_at_ms` is stable).
    Complete(WebPushSubscription),
    /// A pre-token/pre-URL row was re-saved with its token minted (when it
    /// had none) and its absolute URL built: it now sends `Unsubscribe-URL`.
    Healed(WebPushSubscription),
}

/// Backfill the bearer unsubscribe token + absolute URL on a push row that
/// predates them ("send-or-mark"): subscriptions created before the fix
/// round carry no token/URL and send no header — never fabricated at send
/// time. New subscriptions are complete at creation (the endpoint passes the
/// public base URL), and any `create`/`update` re-saves an old row through
/// `upsert_push_subscription`, which mints a missing token and rebuilds the
/// URL. This helper heals one row on demand (wired by the parent snippet
/// into the push show/update paths, which have the base URL): read the
/// token's row; leave complete rows untouched; re-upsert incomplete rows
/// with identical endpoint/keys/policy/alerts plus the base URL. It never
/// creates a row (`Missing` when the token has none).
pub fn backfill_push_unsubscribe(
    store: &dyn PushSubscriptionStore,
    token_id: Id,
    base_url: &str,
    now_ms: u64,
) -> Result<PushBackfillOutcome> {
    let Some(row) = store.push_subscription_by_token(token_id)? else {
        return Ok(PushBackfillOutcome::Missing);
    };
    if row.unsubscribe_url.is_some() {
        return Ok(PushBackfillOutcome::Complete(row));
    }
    let healed = store.upsert_push_subscription(
        row.token_id,
        row.user_id,
        row.endpoint.clone(),
        row.key_p256dh.clone(),
        row.key_auth.clone(),
        row.standard,
        row.policy.clone(),
        row.alerts.clone(),
        Some(base_url),
        now_ms,
    )?;
    Ok(PushBackfillOutcome::Healed(healed))
}
