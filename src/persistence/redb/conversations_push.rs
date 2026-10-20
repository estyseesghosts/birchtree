//! Phase 7 conversation state/mutes and Web Push subscriptions (subagent A).
//!
//! Conversation state and mutes are per-viewer rows; subscriptions are bound
//! to access tokens. One write transaction per mutation; reads are bounded.

use std::collections::HashMap;

use redb::ReadableTable;

use crate::application::conversations::ConversationStore;
use crate::application::push::PushSubscriptionStore;
use crate::domain::{ConversationState, Id, Result, Status, WebPushSubscription};

use super::schema::{
    self, CONVERSATION_MUTES, CONVERSATION_STATE, MENTIONS_BY_ACCOUNT, PUSH_SUBSCRIPTIONS,
    PUSH_SUBSCRIPTIONS_BY_USER, PUSH_SUBSCRIPTION_BY_TOKEN, STATUSES,
};
use super::{decode, encode, RedbStore};

/// Upper bound for the mention-derived candidate scan.
const MENTION_SCAN_CAP: usize = 2_000;

/// Upper bound for one viewer's conversation roots / subscriptions.
const VIEWER_SCAN_CAP: usize = 2_000;

fn state_key(viewer: Id, root: Id) -> [u8; 16] {
    schema::pair_key(viewer.value(), root.value())
}

impl ConversationStore for RedbStore {
    fn conversation_state(&self, viewer: Id, root: Id) -> Result<Option<ConversationState>> {
        let read = self.begin_read()?;
        let table = read.open_table(CONVERSATION_STATE)?;
        Ok(table
            .get(state_key(viewer, root).as_slice())?
            .map(|guard| decode(guard.value()))
            .transpose()?)
    }

    fn conversation_states_for(
        &self,
        viewer: Id,
        roots: &[Id],
    ) -> Result<HashMap<Id, ConversationState>> {
        let read = self.begin_read()?;
        let table = read.open_table(CONVERSATION_STATE)?;
        let mut out = HashMap::with_capacity(roots.len());
        for root in roots {
            if let Some(guard) = table.get(state_key(viewer, *root).as_slice())? {
                out.insert(*root, decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn viewer_conversation_roots(&self, viewer: Id, cap: usize) -> Result<Vec<Id>> {
        let read = self.begin_read()?;
        let table = read.open_table(CONVERSATION_STATE)?;
        let lower = schema::pair_key(viewer.value(), 0);
        let upper = schema::pair_key(viewer.value(), u64::MAX);
        let mut roots = Vec::new();
        for entry in table.range(lower.as_slice()..=upper.as_slice())? {
            let (key, _value) = entry?;
            roots.push(Id::new(u64::from_be_bytes(
                key.value()[8..].try_into().unwrap(),
            )));
            if roots.len() >= cap.min(VIEWER_SCAN_CAP) {
                break;
            }
        }
        Ok(roots)
    }

    fn put_conversation_state(&self, state: ConversationState) -> Result<ConversationState> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(CONVERSATION_STATE)?;
            table.insert(
                state_key(state.viewer_id, state.root_id).as_slice(),
                encode(&state)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(state)
    }

    fn mute_conversation(&self, account: Id, root: Id, now_ms: u64) -> Result<bool> {
        let write = self.begin_write()?;
        let is_new = {
            let mut table = write.open_table(CONVERSATION_MUTES)?;
            let key = state_key(account, root);
            if table.get(key.as_slice())?.is_some() {
                false
            } else {
                table.insert(key.as_slice(), encode(&now_ms)?.as_slice())?;
                true
            }
        };
        write.commit()?;
        Ok(is_new)
    }

    fn unmute_conversation(&self, account: Id, root: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existed = {
            let mut table = write.open_table(CONVERSATION_MUTES)?;
            let key = state_key(account, root);
            let existed = table.get(key.as_slice())?.is_some();
            if existed {
                table.remove(key.as_slice())?;
            }
            existed
        };
        write.commit()?;
        Ok(existed)
    }

    fn conversation_muted(&self, account: Id, root: Id) -> Result<bool> {
        let read = self.begin_read()?;
        let table = read.open_table(CONVERSATION_MUTES)?;
        Ok(table.get(state_key(account, root).as_slice())?.is_some())
    }

    fn conversation_muted_map(
        &self,
        account: Id,
        roots: &[Id],
    ) -> Result<HashMap<Id, bool>> {
        let read = self.begin_read()?;
        let table = read.open_table(CONVERSATION_MUTES)?;
        let mut out = HashMap::with_capacity(roots.len());
        for root in roots {
            out.insert(
                *root,
                table.get(state_key(account, *root).as_slice())?.is_some(),
            );
        }
        Ok(out)
    }

    fn statuses_mentioning(&self, account: Id, limit: usize) -> Result<Vec<Status>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let mentions = read.open_table(MENTIONS_BY_ACCOUNT)?;
        let statuses = read.open_table(STATUSES)?;
        let lower = schema::pair_key(account.value(), 0);
        let upper = schema::pair_key(account.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in mentions.range(lower.as_slice()..=upper.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(
                key.value()[8..].try_into().unwrap(),
            )));
            if ids.len() >= MENTION_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(ids.len());
        ids.sort_by(|a, b| b.cmp(a));
        let mut out = Vec::new();
        for id in ids {
            let Some(guard) = statuses.get(id.value())? else {
                continue;
            };
            let status: Status = decode(guard.value())?;
            drop(guard);
            if !status.is_deleted() {
                out.push(status);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

impl PushSubscriptionStore for RedbStore {
    fn push_subscription_by_token(&self, token_id: Id) -> Result<Option<WebPushSubscription>> {
        let read = self.begin_read()?;
        let by_token = read.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
        let Some(guard) = by_token.get(token_id.value())? else {
            return Ok(None);
        };
        let sub_id = guard.value();
        let subs = read.open_table(PUSH_SUBSCRIPTIONS)?;
        Ok(subs
            .get(sub_id)?
            .map(|guard| decode(guard.value()))
            .transpose()?)
    }

    fn push_subscription_by_id(&self, id: Id) -> Result<Option<WebPushSubscription>> {
        let read = self.begin_read()?;
        let subs = read.open_table(PUSH_SUBSCRIPTIONS)?;
        Ok(subs
            .get(id.value())?
            .map(|guard| decode(guard.value()))
            .transpose()?)
    }

    fn push_subscriptions_for_user(&self, user_id: Id) -> Result<Vec<WebPushSubscription>> {
        let read = self.begin_read()?;
        let by_user = read.open_table(PUSH_SUBSCRIPTIONS_BY_USER)?;
        let subs = read.open_table(PUSH_SUBSCRIPTIONS)?;
        let lower = schema::pair_key(user_id.value(), 0);
        let upper = schema::pair_key(user_id.value(), u64::MAX);
        let mut out = Vec::new();
        for entry in by_user.range(lower.as_slice()..=upper.as_slice())? {
            let (_key, sub_id) = entry?;
            if let Some(guard) = subs.get(sub_id.value())? {
                out.push(decode(guard.value())?);
            }
            if out.len() >= VIEWER_SCAN_CAP {
                break;
            }
        }
        Ok(out)
    }

    fn upsert_push_subscription(
        &self,
        token_id: Id,
        user_id: Id,
        endpoint: String,
        key_p256dh: String,
        key_auth: String,
        standard: bool,
        policy: String,
        alerts: std::collections::HashMap<String, bool>,
        now_ms: u64,
    ) -> Result<WebPushSubscription> {
        let write = self.begin_write()?;
        // Exactly one row per token: drop the token's prior rows first.
        let old: Option<WebPushSubscription> = {
            let by_token = write.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
            match by_token.get(token_id.value())? {
                Some(guard) => {
                    let sub_id = guard.value();
                    let subs = write.open_table(PUSH_SUBSCRIPTIONS)?;
                    subs.get(sub_id)?
                        .map(|guard| decode(guard.value()))
                        .transpose()?
                }
                None => None,
            }
        };
        let row = WebPushSubscription {
            id: old.as_ref().map(|row| row.id).unwrap_or_else(|| self.next_id()),
            token_id,
            user_id,
            endpoint,
            key_p256dh,
            key_auth,
            standard,
            policy,
            alerts,
            created_at_ms: old.as_ref().map(|row| row.created_at_ms).unwrap_or(now_ms),
            updated_at_ms: now_ms,
        };
        {
            let mut subs = write.open_table(PUSH_SUBSCRIPTIONS)?;
            let mut by_token = write.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
            let mut by_user = write.open_table(PUSH_SUBSCRIPTIONS_BY_USER)?;
            if let Some(old) = &old {
                // Remove stale index entries (the token may have moved users).
                let _ = by_user.remove(
                    schema::pair_key(old.user_id.value(), old.id.value()).as_slice(),
                );
            }
            subs.insert(row.id.value(), encode(&row)?.as_slice())?;
            by_token.insert(token_id.value(), row.id.value())?;
            by_user.insert(
                schema::pair_key(user_id.value(), row.id.value()).as_slice(),
                row.id.value(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn update_push_subscription_data(
        &self,
        token_id: Id,
        policy: String,
        alerts: std::collections::HashMap<String, bool>,
        now_ms: u64,
    ) -> Result<Option<WebPushSubscription>> {
        let write = self.begin_write()?;
        let updated = {
            let by_token = write.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
            let Some(guard) = by_token.get(token_id.value())? else {
                return Ok(None);
            };
            let sub_id = guard.value();
            let mut subs = write.open_table(PUSH_SUBSCRIPTIONS)?;
            let Some(guard) = subs.get(sub_id)? else {
                return Ok(None);
            };
            let mut row: WebPushSubscription = decode(guard.value())?;
            drop(guard);
            row.policy = policy;
            row.alerts = alerts;
            row.updated_at_ms = now_ms;
            subs.insert(row.id.value(), encode(&row)?.as_slice())?;
            row
        };
        write.commit()?;
        Ok(Some(updated))
    }

    fn delete_push_subscription(&self, token_id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existed = {
            let mut by_token = write.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
            let Some(guard) = by_token.get(token_id.value())? else {
                return Ok(false);
            };
            let sub_id = guard.value();
            drop(guard);
            let mut subs = write.open_table(PUSH_SUBSCRIPTIONS)?;
            let row: Option<WebPushSubscription> = subs
                .get(sub_id)?
                .map(|guard| decode(guard.value()))
                .transpose()?;
            subs.remove(sub_id)?;
            by_token.remove(token_id.value())?;
            if let Some(row) = row {
                let mut by_user = write.open_table(PUSH_SUBSCRIPTIONS_BY_USER)?;
                let _ = by_user.remove(
                    schema::pair_key(row.user_id.value(), row.id.value()).as_slice(),
                );
            }
            true
        };
        write.commit()?;
        Ok(existed)
    }

    fn delete_push_subscription_by_id(&self, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existed = {
            let mut subs = write.open_table(PUSH_SUBSCRIPTIONS)?;
            let Some(guard) = subs.get(id.value())? else {
                return Ok(false);
            };
            let row: WebPushSubscription = decode(guard.value())?;
            drop(guard);
            subs.remove(id.value())?;
            let mut by_token = write.open_table(PUSH_SUBSCRIPTION_BY_TOKEN)?;
            let _ = by_token.remove(row.token_id.value());
            let mut by_user = write.open_table(PUSH_SUBSCRIPTIONS_BY_USER)?;
            let _ = by_user.remove(
                schema::pair_key(row.user_id.value(), row.id.value()).as_slice(),
            );
            true
        };
        write.commit()?;
        Ok(existed)
    }
}
