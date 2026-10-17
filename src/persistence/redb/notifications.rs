//! Notification record persistence.
//!
//! `FanOutNotification` jobs run after the interaction commits and call
//! [`NotificationStore::create_notification`]; the 40-byte dedup index makes
//! a replayed or retried job a no-op instead of a duplicate record.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewNotification, NotificationStore};
use crate::domain::{Notification, Result};

use super::schema::{self, NOTIFICATIONS, NOTIFICATIONS_BY_RECIPIENT, NOTIFICATION_DEDUP};
use super::{encode, RedbStore, StoreResult};

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
        let write = self.db().begin_write()?;
        let created = create_notification_txn(self, &write, &input)?;
        write.commit()?;
        Ok(created)
    }
}
