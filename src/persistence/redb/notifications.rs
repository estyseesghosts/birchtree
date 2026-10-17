//! Notification record persistence.
//!
//! `FanOutNotification` jobs run after the interaction commits and call
//! [`NotificationStore::create_notification`]; the 40-byte dedup index makes
//! a replayed or retried job a no-op instead of a duplicate record.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewNotification, NotificationStore};
use crate::domain::{Id, Notification, Result};

use super::schema::{self, NOTIFICATIONS, NOTIFICATIONS_BY_RECIPIENT, NOTIFICATION_DEDUP};
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
                matched.push(decode(guard.value())?);
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
        // `id > floor`: start just after the floor's descending key.
        let lower = after
            .map(|floor| schema::pair_key(recipient_id.value(), schema::desc_id(floor.value()) + 1))
            .unwrap_or_else(|| schema::pair_key(recipient_id.value(), 0));
        let hi = schema::pair_key(recipient_id.value(), u64::MAX);
        let mut count = 0u64;
        for entry in index.range(lower.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            let owner = u64::from_be_bytes(key.value()[..8].try_into().unwrap());
            if owner != recipient_id.value() {
                break;
            }
            count += 1;
            if count >= limit {
                break;
            }
        }
        Ok(count)
    }
}
