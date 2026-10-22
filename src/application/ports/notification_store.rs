use crate::domain::{
    Id, Notification, NotificationPolicy, NotificationRequest, NotificationType, Result,
};

/// Notification records (fan-out happens after commit, via jobs).
#[derive(Debug, Clone)]
pub struct NewNotification {
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub created_at_ms: u64,
    /// Upstream `notifications.filtered`: hidden from the index until the
    /// recipient accepts the matching notification request. Defaults to
    /// `false` at construction sites that predate the request pipeline.
    pub filtered: bool,
}

pub trait NotificationStore: Send + Sync {
    /// Insert a notification unless its dedup key `(recipient, type, actor,
    /// status)` already exists; returns `None` on a dedup hit. Used by the
    /// `FanOutNotification` job handler, so job retries are harmless.
    fn create_notification(&self, input: NewNotification) -> Result<Option<Notification>>;

    /// One page for the recipient, newest first; cursors exclusive
    /// (`id < max_id`, `id > since_id`/`id > min_id`), bounded scan.
    /// Filtered rows are excluded (upstream's default notification scope):
    /// they surface only through notification requests until accepted.
    fn notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>>;

    /// Filtered rows for `include_filtered=true`, newest first with the same
    /// exclusive cursors (upstream `browserable` serves them when the flag is
    /// set). Bounded scan like `notifications_page`.
    fn filtered_notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>>;

    fn notification_by_id(&self, id: Id) -> Result<Option<Notification>>;

    /// Delete one of the recipient's notifications; `false` when missing or
    /// belonging to someone else.
    fn delete_notification(&self, recipient_id: Id, id: Id) -> Result<bool>;

    /// Delete every notification of the recipient.
    fn clear_notifications(&self, recipient_id: Id) -> Result<()>;

    /// Count notifications with `id > floor`, capped by `limit` (the
    /// marker-based `unread_count`). Filtered rows are excluded, like the
    /// index page.
    fn count_notifications_after(
        &self,
        recipient_id: Id,
        floor: Option<Id>,
        limit: u64,
    ) -> Result<u64>;

    // -- Notification-request pipeline (round 3, area C) -------------------
    // Upstream: `NotifyService` filters into `notification_requests`;
    // `AcceptNotificationRequestService` unhides + emits
    // `notifications_merged`; dismiss deletes the filtered rows.

    /// The recipient's policy, or the upstream-column defaults when no row
    /// was ever stored (`find_or_initialize_by`).
    fn notification_policy(&self, account_id: Id) -> Result<NotificationPolicy>;

    /// Replace the recipient's policy row in ONE transaction.
    fn set_notification_policy(&self, policy: &NotificationPolicy) -> Result<NotificationPolicy>;

    /// Create or refresh the `(recipient, actor)` request row (upstream
    /// `find_or_initialize_by` + `last_status_id` bump): the newest filtered
    /// mention/quote status wins, `updated_at` moves, the row id is stable.
    /// `None` status ids never create a row (upstream only files requests
    /// for mention/quote notifications, which always carry a status).
    fn upsert_notification_request(
        &self,
        recipient_id: Id,
        from_account_id: Id,
        last_status_id: Id,
        now_ms: u64,
    ) -> Result<NotificationRequest>;

    /// Every request row of the recipient, newest first, bounded scan.
    fn notification_requests(&self, recipient_id: Id) -> Result<Vec<NotificationRequest>>;

    /// One request row of the recipient (`None` when missing or foreign).
    fn notification_request(&self, recipient_id: Id, id: Id)
    -> Result<Option<NotificationRequest>>;

    /// Delete one request row of the recipient; `false` when missing or
    /// foreign (accept/dismiss destroy the row after doing their work).
    fn delete_notification_request(&self, recipient_id: Id, id: Id) -> Result<bool>;

    /// How many *filtered* notification rows the recipient still holds from
    /// this actor (the request DTO `notifications_count`, computed live so
    /// it can never drift from the rows).
    fn count_filtered_notifications(&self, recipient_id: Id, from_account_id: Id) -> Result<u64>;

    /// Unhide every filtered notification from this actor (accept path);
    /// returns how many rows flipped. One transaction.
    fn unfilter_notifications(&self, recipient_id: Id, from_account_id: Id) -> Result<u64>;

    /// Delete every *filtered* notification from this actor (dismiss path);
    /// returns how many rows were removed. Unfiltered rows are kept.
    fn delete_filtered_notifications(&self, recipient_id: Id, from_account_id: Id) -> Result<u64>;

    /// Record that future notifications from this actor bypass the filter
    /// (upstream `NotificationPermission`, created on accept). Idempotent.
    fn grant_notification_permission(&self, recipient_id: Id, from_account_id: Id) -> Result<()>;

    /// Whether such a permission row exists (upstream
    /// `FilterCondition#override_for_sender?`).
    fn has_notification_permission(&self, recipient_id: Id, from_account_id: Id) -> Result<bool>;
}
