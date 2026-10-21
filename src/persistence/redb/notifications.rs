//! Notification record persistence.
//!
//! `FanOutNotification` jobs run after the interaction commits and call
//! [`NotificationStore::create_notification`]; the 40-byte dedup index makes
//! a replayed or retried job a no-op instead of a duplicate record.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewNotification, NotificationStore};
use crate::domain::{Id, Notification, NotificationPolicy, NotificationRequest, Result};

use super::schema::{
    self, NOTIFICATIONS, NOTIFICATIONS_BY_RECIPIENT, NOTIFICATION_DEDUP,
    NOTIFICATION_PERMISSIONS, NOTIFICATION_POLICIES, NOTIFICATION_REQUESTS,
    NOTIFICATION_REQUESTS_BY_PAIR,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Scan cap for one notification page (bounds a pathological recipient).
const PAGE_SCAN_CAP: usize = 1_000;

/// Insert one notification row plus its recipient index (and, when the input
/// carries an actor, its dedup record) inside the caller's ONE transaction.
///
/// The dedup check and the insert happen in the same transaction, so two
/// concurrent retries of the same job cannot both observe "absent" and both
/// insert. The id is minted only after the dedup check passes, so a dedup hit
/// costs no id.
fn create_notification_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    input: &NewNotification,
) -> StoreResult<Option<Notification>> {
    // The dedup key scopes to one interaction *instance*: same recipient,
    // type, actor, status and interaction timestamp.
    let dedup_key = input.actor_id.map(|actor| {
        schema::notification_dedup_key(
            input.recipient_id.value(),
            input.r#type.ord(),
            actor.value(),
            input.status_id.map(|s| s.value()).unwrap_or(0),
            input.created_at_ms,
        )
    });
    if let Some(key) = &dedup_key {
        let dedup = write.open_table(NOTIFICATION_DEDUP)?;
        if dedup.get(key.as_slice())?.is_some() {
            return Ok(None); // Dedup hit: the record already exists.
        }
    }

    let id = store.next_id();
    let notification = Notification {
        id,
        recipient_id: input.recipient_id,
        r#type: input.r#type,
        actor_id: input.actor_id,
        status_id: input.status_id,
        read: false,
        filtered: input.filtered,
        created_at_ms: input.created_at_ms,
    };
    {
        let mut notifications = write.open_table(NOTIFICATIONS)?;
        notifications.insert(id.value(), encode(&notification)?.as_slice())?;
    }
    {
        // Descending-id recipient index: a forward scan over
        // `(recipient || desc(id))` yields newest first.
        let mut by_recipient = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
        by_recipient.insert(
            schema::pair_key(input.recipient_id.value(), schema::desc_id(id.value())).as_slice(),
            id.value(),
        )?;
    }
    if let Some(key) = &dedup_key {
        let mut dedup = write.open_table(NOTIFICATION_DEDUP)?;
        dedup.insert(key.as_slice(), id.value())?;
    }
    Ok(Some(notification))
}

impl NotificationStore for RedbStore {
    fn create_notification(&self, input: NewNotification) -> Result<Option<Notification>> {
        let write = self.begin_write()?;
        let created = create_notification_txn(self, &write, &input)?;
        write.commit()?;
        Ok(created)
    }

    fn notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if max_id.is_some_and(|m| m.value() == 0) {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let index = read.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
        let rows = read.open_table(NOTIFICATIONS)?;
        // Cursor mapping over `(recipient || desc(id))`: `id < max_id`,
        // `id > since_id`/`id > min_id` (exclusive, like Mastodon).
        let lower = max_id
            .map(|m| schema::pair_key(recipient_id.value(), schema::desc_id(m.value()) + 1))
            .unwrap_or_else(|| schema::pair_key(recipient_id.value(), 0));
        let upper_exclusive = since_id
            .or(min_id)
            .map(|x| schema::desc_id(x.value()));
        let want_oldest = min_id.is_some() && since_id.is_none();
        let mut matched: Vec<Notification> = Vec::new();
        let mut scanned = 0usize;
        let entries = match upper_exclusive {
            Some(upper) => {
                let end = schema::pair_key(recipient_id.value(), upper);
                index.range(lower.as_slice()..end.as_slice())?
            }
            None => {
                let end = schema::pair_key(recipient_id.value(), u64::MAX);
                index.range(lower.as_slice()..=end.as_slice())?
            }
        };
        for entry in entries {
            let (key, value) = entry?;
            scanned += 1;
            // Defensive: never leak another recipient's rows.
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            let id = Id::new(value.value());
            if let Some(guard) = rows.get(id.value())? {
                // Filtered rows are hidden from the index (upstream's default
                // notification scope); they surface through requests instead.
                let notification: Notification = decode(guard.value())?;
                if !notification.filtered {
                    matched.push(notification);
                }
            }
            if (!want_oldest && matched.len() >= limit) || scanned >= PAGE_SCAN_CAP {
                break;
            }
        }
        if want_oldest && matched.len() > limit {
            matched.drain(..matched.len() - limit);
        }
        self.note_scanned(scanned);
        Ok(matched)
    }

    fn filtered_notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if max_id.is_some_and(|m| m.value() == 0) {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let index = read.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
        let rows = read.open_table(NOTIFICATIONS)?;
        // Same cursor mapping as `notifications_page`, filter flipped: only
        // filtered rows (upstream `browserable` with `include_filtered`).
        let lower = max_id
            .map(|m| schema::pair_key(recipient_id.value(), schema::desc_id(m.value()) + 1))
            .unwrap_or_else(|| schema::pair_key(recipient_id.value(), 0));
        let upper_exclusive = since_id
            .or(min_id)
            .map(|x| schema::desc_id(x.value()));
        let want_oldest = min_id.is_some() && since_id.is_none();
        let mut matched: Vec<Notification> = Vec::new();
        let mut scanned = 0usize;
        let entries = match upper_exclusive {
            Some(upper) => {
                let end = schema::pair_key(recipient_id.value(), upper);
                index.range(lower.as_slice()..end.as_slice())?
            }
            None => {
                let end = schema::pair_key(recipient_id.value(), u64::MAX);
                index.range(lower.as_slice()..=end.as_slice())?
            }
        };
        for entry in entries {
            let (key, value) = entry?;
            scanned += 1;
            // Defensive: never leak another recipient's rows.
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            let id = Id::new(value.value());
            if let Some(guard) = rows.get(id.value())? {
                let notification: Notification = decode(guard.value())?;
                if notification.filtered {
                    matched.push(notification);
                }
            }
            if (!want_oldest && matched.len() >= limit) || scanned >= PAGE_SCAN_CAP {
                break;
            }
        }
        if want_oldest && matched.len() > limit {
            matched.drain(..matched.len() - limit);
        }
        self.note_scanned(scanned);
        Ok(matched)
    }

    fn notification_by_id(&self, id: Id) -> Result<Option<Notification>> {
        let read = self.begin_read()?;
        let table = read.open_table(NOTIFICATIONS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn delete_notification(&self, recipient_id: Id, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existing: Option<Notification> = {
            let table = write.open_table(NOTIFICATIONS)?;
            match table.get(id.value())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        let Some(notification) = existing else {
            return Ok(false);
        };
        if notification.recipient_id != recipient_id {
            return Ok(false);
        }
        {
            let mut table = write.open_table(NOTIFICATIONS)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
            table.remove(
                schema::pair_key(recipient_id.value(), schema::desc_id(id.value())).as_slice(),
            )?;
        }
        write.commit()?;
        Ok(true)
    }

    fn clear_notifications(&self, recipient_id: Id) -> Result<()> {
        // ONE transaction: collect the recipient's ids from the index, then
        // delete rows + index entries.
        let write = self.begin_write()?;
        let ids: Vec<Id> = {
            let index = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
            let lo = schema::pair_key(recipient_id.value(), 0);
            let hi = schema::pair_key(recipient_id.value(), u64::MAX);
            let mut ids = Vec::new();
            for entry in index.range(lo.as_slice()..=hi.as_slice())? {
                let (_key, value) = entry?;
                ids.push(Id::new(value.value()));
                if ids.len() >= PAGE_SCAN_CAP * 10 {
                    break;
                }
            }
            ids
        };
        {
            let mut table = write.open_table(NOTIFICATIONS)?;
            for id in &ids {
                table.remove(id.value())?;
            }
        }
        {
            let mut table = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
            for id in &ids {
                table.remove(
                    schema::pair_key(recipient_id.value(), schema::desc_id(id.value())).as_slice(),
                )?;
            }
        }
        self.note_scanned(ids.len());
        write.commit()?;
        Ok(())
    }

    fn count_notifications_after(
        &self,
        recipient_id: Id,
        after: Option<Id>,
        limit: u64,
    ) -> Result<u64> {
        let read = self.begin_read()?;
        let index = read.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
        let rows = read.open_table(NOTIFICATIONS)?;
        // `id > floor`: start just after the floor's descending key.
        let lower = after
            .map(|floor| schema::pair_key(recipient_id.value(), schema::desc_id(floor.value()) + 1))
            .unwrap_or_else(|| schema::pair_key(recipient_id.value(), 0));
        let hi = schema::pair_key(recipient_id.value(), u64::MAX);
        let mut count = 0u64;
        let mut scanned = 0usize;
        for entry in index.range(lower.as_slice()..=hi.as_slice())? {
            let (key, value) = entry?;
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            scanned += 1;
            if scanned > PAGE_SCAN_CAP * 10 {
                break;
            }
            // Filtered rows are excluded, like the index page.
            let visible = match rows.get(value.value())? {
                Some(guard) => {
                    let notification: Notification = decode(guard.value())?;
                    !notification.filtered
                }
                None => false,
            };
            if !visible {
                continue;
            }
            count += 1;
            if count >= limit {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(count)
    }

    // -- Notification-request pipeline (round 3, area C) ------------------

    fn notification_policy(&self, account_id: Id) -> Result<NotificationPolicy> {
        let read = self.begin_read()?;
        let table = match read.open_table(NOTIFICATION_POLICIES) {
            Ok(table) => table,
            // Fresh databases (and databases opened before this table
            // existed) have no policy table yet: the upstream column
            // defaults apply.
            Err(redb::TableError::TableDoesNotExist(_)) => {
                return Ok(NotificationPolicy::new(account_id));
            }
            Err(err) => return Err(err.into()),
        };
        match table.get(account_id.value())? {
            Some(guard) => Ok(decode(guard.value())?),
            None => Ok(NotificationPolicy::new(account_id)),
        }
    }

    fn set_notification_policy(&self, policy: &NotificationPolicy) -> Result<NotificationPolicy> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(NOTIFICATION_POLICIES)?;
            table.insert(policy.account_id.value(), encode(policy)?.as_slice())?;
        }
        write.commit()?;
        Ok(policy.clone())
    }

    fn upsert_notification_request(
        &self,
        recipient_id: Id,
        from_account_id: Id,
        last_status_id: Id,
        now_ms: u64,
    ) -> Result<NotificationRequest> {
        // ONE transaction: find the live row by pair key, or mint one.
        let write = self.begin_write()?;
        let pair = schema::pair_key(recipient_id.value(), from_account_id.value());
        let existing_id: Option<Id> = {
            let index = write.open_table(NOTIFICATION_REQUESTS_BY_PAIR)?;
            index.get(pair.as_slice())?.map(|guard| Id::new(guard.value()))
        };
        let request = match existing_id {
            Some(id) => {
                let stored: Option<NotificationRequest> = {
                    let table = write.open_table(NOTIFICATION_REQUESTS)?;
                    match table.get(id.value())? {
                        Some(guard) => Some(decode(guard.value())?),
                        None => None,
                    }
                };
                // Index pointed at a missing row: re-mint below.
                let mut request = stored.unwrap_or_else(|| NotificationRequest {
                    id: self.next_id(),
                    account_id: recipient_id,
                    from_account_id,
                    last_status_id: Some(last_status_id),
                    notifications_count: 0,
                    created_at_ms: now_ms,
                    updated_at_ms: now_ms,
                });
                request.last_status_id = Some(last_status_id);
                request.updated_at_ms = now_ms;
                let mut table = write.open_table(NOTIFICATION_REQUESTS)?;
                table.insert(request.id.value(), encode(&request)?.as_slice())?;
                request
            }
            None => {
                let request = NotificationRequest {
                    id: self.next_id(),
                    account_id: recipient_id,
                    from_account_id,
                    last_status_id: Some(last_status_id),
                    notifications_count: 0,
                    created_at_ms: now_ms,
                    updated_at_ms: now_ms,
                };
                {
                    let mut table = write.open_table(NOTIFICATION_REQUESTS)?;
                    table.insert(request.id.value(), encode(&request)?.as_slice())?;
                }
                {
                    let mut index = write.open_table(NOTIFICATION_REQUESTS_BY_PAIR)?;
                    index.insert(pair.as_slice(), request.id.value())?;
                }
                request
            }
        };
        write.commit()?;
        Ok(request)
    }

    fn notification_requests(&self, recipient_id: Id) -> Result<Vec<NotificationRequest>> {
        let read = self.begin_read()?;
        let index = match read.open_table(NOTIFICATION_REQUESTS_BY_PAIR) {
            Ok(index) => index,
            // No request was ever filed: honestly empty.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let rows = read.open_table(NOTIFICATION_REQUESTS)?;
        let lo = schema::pair_key(recipient_id.value(), 0);
        let hi = schema::pair_key(recipient_id.value(), u64::MAX);
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = entry?;
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            if let Some(guard) = rows.get(value.value())? {
                let request: NotificationRequest = decode(guard.value())?;
                if request.account_id == recipient_id {
                    out.push(request);
                }
            }
        }
        // Newest first (request ids are timestamp-ordered).
        out.sort_by_key(|request| std::cmp::Reverse(request.id.value()));
        self.note_scanned(scanned);
        Ok(out)
    }

    fn notification_request(
        &self,
        recipient_id: Id,
        id: Id,
    ) -> Result<Option<NotificationRequest>> {
        let read = self.begin_read()?;
        let table = match read.open_table(NOTIFICATION_REQUESTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        match table.get(id.value())? {
            Some(guard) => {
                let request: NotificationRequest = decode(guard.value())?;
                Ok((request.account_id == recipient_id).then_some(request))
            }
            None => Ok(None),
        }
    }

    fn delete_notification_request(&self, recipient_id: Id, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existing: Option<NotificationRequest> = {
            let table = write.open_table(NOTIFICATION_REQUESTS)?;
            match table.get(id.value())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        let Some(request) = existing else {
            return Ok(false);
        };
        if request.account_id != recipient_id {
            return Ok(false);
        }
        {
            let mut table = write.open_table(NOTIFICATION_REQUESTS)?;
            table.remove(id.value())?;
        }
        {
            let mut index = write.open_table(NOTIFICATION_REQUESTS_BY_PAIR)?;
            index.remove(
                schema::pair_key(recipient_id.value(), request.from_account_id.value()).as_slice(),
            )?;
        }
        write.commit()?;
        Ok(true)
    }

    fn count_filtered_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64> {
        let read = self.begin_read()?;
        let index = read.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
        let rows = read.open_table(NOTIFICATIONS)?;
        let mut count = 0u64;
        let mut scanned = 0usize;
        let lo = schema::pair_key(recipient_id.value(), 0);
        let hi = schema::pair_key(recipient_id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = entry?;
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            scanned += 1;
            if scanned > PAGE_SCAN_CAP * 10 {
                break;
            }
            if let Some(guard) = rows.get(value.value())? {
                let notification: Notification = decode(guard.value())?;
                if notification.filtered && notification.actor_id == Some(from_account_id) {
                    count += 1;
                }
            }
        }
        self.note_scanned(scanned);
        Ok(count)
    }

    fn unfilter_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64> {
        // ONE transaction: flip every filtered row of this pair.
        let write = self.begin_write()?;
        // Candidate ids are read first; every row is re-verified inside the
        // write transaction below, so a concurrent flip cannot resurrect or
        // lose a row.
        let ids: Vec<Id> = filtered_ids_from_actor(self, recipient_id, from_account_id)?;
        let mut flipped = 0u64;
        {
            let mut table = write.open_table(NOTIFICATIONS)?;
            for id in &ids {
                let mut notification: Notification = match table.get(id.value())? {
                    Some(guard) => decode(guard.value())?,
                    None => continue,
                };
                if notification.recipient_id != recipient_id
                    || notification.actor_id != Some(from_account_id)
                    || !notification.filtered
                {
                    continue;
                }
                notification.filtered = false;
                table.insert(id.value(), encode(&notification)?.as_slice())?;
                flipped += 1;
            }
        }
        write.commit()?;
        Ok(flipped)
    }

    fn delete_filtered_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64> {
        // ONE transaction: remove every filtered row of this pair (rows +
        // recipient index). Unfiltered rows are kept.
        let write = self.begin_write()?;
        // Candidate ids are read first; every row is re-verified inside the
        // write transaction below, so a concurrent flip cannot resurrect or
        // lose a row.
        let ids: Vec<Id> = filtered_ids_from_actor(self, recipient_id, from_account_id)?;
        let mut removed = 0u64;
        {
            let mut table = write.open_table(NOTIFICATIONS)?;
            let mut index = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
            for id in &ids {
                let existing: Option<Notification> = match table.get(id.value())? {
                    Some(guard) => Some(decode(guard.value())?),
                    None => None,
                };
                let Some(notification) = existing else {
                    continue;
                };
                if notification.recipient_id != recipient_id
                    || notification.actor_id != Some(from_account_id)
                    || !notification.filtered
                {
                    continue;
                }
                table.remove(id.value())?;
                index.remove(
                    schema::pair_key(recipient_id.value(), schema::desc_id(id.value())).as_slice(),
                )?;
                removed += 1;
            }
        }
        write.commit()?;
        Ok(removed)
    }

    fn grant_notification_permission(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(NOTIFICATION_PERMISSIONS)?;
            table.insert(
                schema::pair_key(recipient_id.value(), from_account_id.value()).as_slice(),
                0u64,
            )?;
        }
        write.commit()?;
        Ok(())
    }

    fn has_notification_permission(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<bool> {
        let read = self.begin_read()?;
        let table = match read.open_table(NOTIFICATION_PERMISSIONS) {
            Ok(table) => table,
            // No accept was ever granted.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        Ok(table
            .get(schema::pair_key(recipient_id.value(), from_account_id.value()).as_slice())?
            .is_some())
    }
}

/// Collect ids of filtered notification rows of `(recipient, actor)` by
/// scanning the recipient index (bounded: `PAGE_SCAN_CAP * 10`, the same
/// bound `clear_notifications` uses). Callers re-verify every row inside
/// their write transaction.
fn filtered_ids_from_actor(
    store: &RedbStore,
    recipient_id: Id,
    from_account_id: Id,
) -> StoreResult<Vec<Id>> {
    let read = store.begin_read()?;
    let index = read.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
    let rows = read.open_table(NOTIFICATIONS)?;
    let lo = schema::pair_key(recipient_id.value(), 0);
    let hi = schema::pair_key(recipient_id.value(), u64::MAX);
    let mut ids = Vec::new();
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        let (key, value) = entry?;
        let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
        if owner != recipient_id.value() {
            break;
        }
        if ids.len() >= PAGE_SCAN_CAP * 10 {
            break;
        }
        let id = Id::new(value.value());
        match rows.get(id.value())? {
            Some(guard) => {
                let notification: Notification = decode(guard.value())?;
                if notification.filtered && notification.actor_id == Some(from_account_id) {
                    ids.push(id);
                }
            }
            None => continue,
        }
    }
    store.note_scanned(ids.len());
    Ok(ids)
}
