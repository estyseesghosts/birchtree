//! Federation persistence: actor keys, the actor-URI index, remote accounts,
//! the inbound activity dedup record, and the inbound and outbound follow
//! commands. Each command is ONE `redb` transaction (`birchtree.md` §5): the
//! dedup record, the state change, its counters and its durable jobs commit or
//! abort together. No network I/O happens here.

use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::application::federation_ports::{
    DeliverySpec, FederationStore, InboundDisposition, InboundEffect, RemoteAccountUpsert,
};
use crate::application::notifications::fan_out_payload;
use crate::domain::{
    Account, ActorKeys, DeliveryDestState, Error, Follow, FollowState, Id, Job, JobKind, NotificationType, Result,
};

use super::relationships::{read_follow_txn, read_stats_txn, write_stats_txn};
use super::schema::{
    self, ACCOUNTS, ACCOUNT_BY_ACTOR_URI, ACTIVITY_DEDUP, ACTOR_KEYS, DELIVERY_DEST,
    FOLLOWS, FOLLOWS_BY_FOLLOWED, FOLLOW_BY_URI, HANDLE_INDEX,
};
use super::{decode, encode, RedbStore, StoreResult};

/// The key record stored per account. Remote accounts store no private key.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredKeys {
    key_id: String,
    public_key_pem: String,
    private_key_pem: Option<String>,
    /// Absent in records written before the field existed (treated as unknown age).
    #[serde(default)]
    refreshed_at_ms: Option<u64>,
}

impl StoredKeys {
    fn into_domain(self, account_id: Id) -> ActorKeys {
        ActorKeys {
            account_id,
            key_id: self.key_id,
            public_key_pem: self.public_key_pem,
            private_key_pem: self.private_key_pem,
            refreshed_at_ms: self.refreshed_at_ms,
        }
    }
}

/// Read an account row inside an existing transaction.
fn account_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Account>> {
    let accounts = write.open_table(ACCOUNTS)?;
    match accounts.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Build the durable `DeliverActivity` job for a delivery spec. The payload
/// carries the exact JSON, so a retry sends the same bytes.
pub(super) fn delivery_job(job_id: Id, spec: &DeliverySpec, now_ms: u64) -> Job {
    Job::new(
        job_id,
        JobKind::DeliverActivity,
        serde_json::json!({
            "sender_id": spec.sender_id.value(),
            "inbox": spec.inbox_uri,
            "activity_uri": spec.activity_uri,
            "body": spec.body,
        }),
        now_ms,
        now_ms,
    )
}

/// Insert or update the remote account for `actor` inside an existing
/// transaction. Keyed by actor URI; the handle index is kept in step.
fn upsert_remote_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    actor: &RemoteAccountUpsert,
    now_ms: u64,
) -> Result<Account> {
    let normalized = crate::domain::account::normalize_username(&actor.username);
    let handle = crate::domain::account::handle_key(&normalized, &actor.domain);
    let existing_id = {
        let index = write.open_table(ACCOUNT_BY_ACTOR_URI)?;
        let found = index.get(actor.actor_uri.as_str())?.map(|g| g.value());
        found
    };

    let mut account = match existing_id {
        Some(id) => {
            let Some(mut account) = account_txn(write, Id::new(id))? else {
                return Err(Error::Storage(format!("dangling actor index for {}", actor.actor_uri)));
            };
            if account.normalized_username != normalized || account.domain != actor.domain {
                // The actor changed its handle: move the index entry.
                let mut handles = write.open_table(HANDLE_INDEX)?;
                let old = crate::domain::account::handle_key(&account.normalized_username, &account.domain);
                handles.remove(old.as_str())?;
                handles.insert(handle.as_str(), account.id.value())?;
                account.normalized_username = normalized.clone();
                account.domain = actor.domain.clone();
            }
            account
        }
        None => {
            {
                let handles = write.open_table(HANDLE_INDEX)?;
                if handles.get(handle.as_str())?.is_some() {
                    return Err(Error::Conflict(format!("handle already registered: {handle}")));
                }
            }
            Account {
                id: store.next_id(),
                username: actor.username.clone(),
                normalized_username: normalized.clone(),
                domain: actor.domain.clone(),
                display_name: String::new(),
                note: String::new(),
                actor_uri: actor.actor_uri.clone(),
                inbox_uri: None,
                shared_inbox_uri: None,
                local: false,
                locked: false,
                suspended: false,
                silenced: false,
                sensitized: false,
                moved_to: None,
                bot: false,
                discoverable: None,
                indexable: false,
                hide_collections: None,
                fields: Vec::new(),
                attribution_domains: Vec::new(),
                created_at_ms: now_ms,
            }
        }
    };

    account.username = actor.username.clone();
    account.display_name = actor.display_name.clone();
    account.note = actor.note.clone();
    account.locked = actor.locked;
    account.inbox_uri = Some(actor.inbox_uri.clone());
    account.shared_inbox_uri = actor.shared_inbox_uri.clone();

    {
        let mut accounts = write.open_table(ACCOUNTS)?;
        accounts.insert(account.id.value(), encode(&account)?.as_slice())?;
    }
    {
        let mut handles = write.open_table(HANDLE_INDEX)?;
        handles.insert(handle.as_str(), account.id.value())?;
    }
    {
        let mut index = write.open_table(ACCOUNT_BY_ACTOR_URI)?;
        index.insert(actor.actor_uri.as_str(), account.id.value())?;
    }
    {
        let stored = StoredKeys {
            key_id: actor.key_id.clone(),
            public_key_pem: actor.public_key_pem.clone(),
            private_key_pem: None,
            refreshed_at_ms: Some(now_ms),
        };
        let mut keys = write.open_table(ACTOR_KEYS)?;
        // A remote key is replaced when the actor rotates it. A local key is
        // never replaced by a remote upsert: the actor URI check above keeps
        // a local account out of this path.
        keys.insert(account.id.value(), encode(&stored)?.as_slice())?;
    }
    Ok(account)
}

impl FederationStore for RedbStore {
    fn actor_keys(&self, account_id: Id) -> Result<Option<ActorKeys>> {
        let read = self.begin_read()?;
        let table = read.open_table(ACTOR_KEYS)?;
        match table.get(account_id.value())? {
            Some(guard) => {
                let stored: StoredKeys = decode(guard.value())?;
                Ok(Some(stored.into_domain(account_id)))
            }
            None => Ok(None),
        }
    }

    fn store_local_keys(
        &self,
        account_id: Id,
        key_id: &str,
        public_key_pem: &str,
        private_key_pem: &str,
    ) -> Result<ActorKeys> {
        let write = self.begin_write()?;
        let stored = {
            let mut keys = write.open_table(ACTOR_KEYS)?;
            let existing = match keys.get(account_id.value())? {
                Some(guard) => Some(decode::<StoredKeys>(guard.value())?),
                None => None,
            };
            match existing {
                Some(existing) => existing,
                None => {
                    let fresh = StoredKeys {
                        key_id: key_id.to_string(),
                        public_key_pem: public_key_pem.to_string(),
                        private_key_pem: Some(private_key_pem.to_string()),
                        refreshed_at_ms: None,
                    };
                    keys.insert(account_id.value(), encode(&fresh)?.as_slice())?;
                    fresh
                }
            }
        };
        write.commit()?;
        Ok(stored.into_domain(account_id))
    }

    fn account_by_actor_uri(&self, actor_uri: &str) -> Result<Option<Account>> {
        let read = self.begin_read()?;
        let index = read.open_table(ACCOUNT_BY_ACTOR_URI)?;
        let Some(id) = index.get(actor_uri)?.map(|g| g.value()) else {
            return Ok(None);
        };
        let accounts = read.open_table(ACCOUNTS)?;
        match accounts.get(id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn activity_seen(&self, activity_uri: &str) -> Result<bool> {
        let read = self.begin_read()?;
        let table = read.open_table(ACTIVITY_DEDUP)?;
        Ok(table.get(activity_uri)?.is_some())
    }

    fn delivery_dest(&self, host: &str) -> Result<Option<DeliveryDestState>> {
        let read = self.begin_read()?;
        let table = read.open_table(DELIVERY_DEST)?;
        match table.get(host)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn set_delivery_dest(&self, host: &str, state: DeliveryDestState) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(DELIVERY_DEST)?;
            table.insert(host, encode(&state)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn commit_inbound(
        &self,
        activity_uri: &str,
        actor: RemoteAccountUpsert,
        effect: InboundEffect,
        now_ms: u64,
    ) -> Result<InboundDisposition> {
        let write = self.begin_write()?;
        {
            // The dedup record commits with the effect. A replay finds it and
            // returns before touching anything else.
            let mut dedup = write.open_table(ACTIVITY_DEDUP)?;
            if dedup.get(activity_uri)?.is_some() {
                return Ok(InboundDisposition::Duplicate);
            }
            dedup.insert(activity_uri, now_ms)?;
        }

        let remote = upsert_remote_txn(self, &write, &actor, now_ms)?;
        let disposition = match effect {
            InboundEffect::Follow {
                target_id,
                follow_uri,
                accept,
            } => {
                let Some(target) = account_txn(&write, target_id)? else {
                    return Err(Error::NotFound(format!("account {target_id}")));
                };
                if !target.local {
                    return Err(Error::InvalidInput("follow target is not local".into()));
                }
                self.apply_inbound_follow(&write, &remote, &target, &follow_uri, accept, now_ms)?
            }
            InboundEffect::Accept { follow_uri } => {
                self.apply_inbound_accept(&write, &remote, &follow_uri)?
            }
            InboundEffect::Reject { follow_uri } => {
                self.apply_inbound_reject(&write, &remote, &follow_uri)?
            }
            InboundEffect::UndoFollow { target_id } => {
                match super::relationships::unfollow_txn(&write, remote.id, target_id)? {
                    Some(removed) => {
                        if let Some(uri) = removed.uri.as_deref() {
                            let mut by_uri = write.open_table(FOLLOW_BY_URI)?;
                            by_uri.remove(uri)?;
                        }
                        InboundDisposition::Applied
                    }
                    None => InboundDisposition::Ignored("no follow to undo".into()),
                }
            }
        };
        // An activity with no effect is not recorded. Dropping the transaction
        // rolls back its dedup record too. A redelivery is then evaluated again,
        // which is safe because nothing changed. Mastodon can reuse an Undo's ID
        // for a later undo of the same follow row, so recording an ignored one
        // would wrongly swallow the real Undo.
        if matches!(disposition, InboundDisposition::Ignored(_)) {
            return Ok(disposition);
        }
        write.commit()?;
        Ok(disposition)
    }

    fn request_follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        follow_uri: &str,
        delivery: DeliverySpec,
        now_ms: u64,
    ) -> Result<Follow> {
        if follower_id == followed_id {
            return Err(Error::Forbidden("Following your own account is not allowed".into()));
        }
        let write = self.begin_write()?;
        let key = schema::pair_key(follower_id.value(), followed_id.value());
        if let Some(existing) = read_follow_txn(&write, key.as_slice())? {
            // A pending or accepted row already exists: no second request.
            return Ok(existing);
        }

        let follow = Follow {
            follower_id,
            followed_id,
            show_reblogs: true,
            notify: false,
            languages: None,
            state: FollowState::Pending,
            created_at_ms: now_ms,
            uri: Some(follow_uri.to_string()),
        };
        let mut followed_stats = read_stats_txn(&write, followed_id)?;
        followed_stats.follow_requests_count += 1;
        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.insert(key.as_slice(), encode(&follow)?.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed.insert(schema::pair_key(followed_id.value(), follower_id.value()).as_slice(), ())?;
        }
        {
            let mut by_uri = write.open_table(FOLLOW_BY_URI)?;
            by_uri.insert(follow_uri, key.as_slice())?;
        }
        write_stats_txn(&write, followed_id, &followed_stats)?;
        let job = delivery_job(self.next_id(), &delivery, now_ms);
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(follow)
    }

    fn upsert_remote(&self, actor: RemoteAccountUpsert, now_ms: u64) -> Result<Account> {
        let write = self.begin_write()?;
        let account = upsert_remote_txn(self, &write, &actor, now_ms)?;
        write.commit()?;
        Ok(account)
    }

    fn undo_follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        delivery: DeliverySpec,
        now_ms: u64,
    ) -> Result<Option<Follow>> {
        let write = self.begin_write()?;
        let Some(removed) = super::relationships::unfollow_txn(&write, follower_id, followed_id)? else {
            return Ok(None);
        };
        if let Some(uri) = removed.uri.as_deref() {
            // The follow-by-URI index dies with the follow row it points at.
            let mut by_uri = write.open_table(FOLLOW_BY_URI)?;
            by_uri.remove(uri)?;
        }
        let job = delivery_job(self.next_id(), &delivery, now_ms);
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(Some(removed))
    }

    fn follow_by_uri(&self, follow_uri: &str) -> Result<Option<Follow>> {
        let read = self.begin_read()?;
        let by_uri = read.open_table(FOLLOW_BY_URI)?;
        let Some(key) = by_uri.get(follow_uri)?.map(|g| g.value().to_vec()) else {
            return Ok(None);
        };
        let follows = read.open_table(FOLLOWS)?;
        match follows.get(key.as_slice())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }
}

impl RedbStore {
    /// Apply an inbound Follow inside the caller's transaction.
    fn apply_inbound_follow(
        &self,
        write: &WriteTransaction,
        remote: &Account,
        target: &Account,
        follow_uri: &str,
        accept: DeliverySpec,
        now_ms: u64,
    ) -> Result<InboundDisposition> {
        if remote.id == target.id {
            return Err(Error::Forbidden("Following your own account is not allowed".into()));
        }
        let key = schema::pair_key(remote.id.value(), target.id.value());
        let old_state = read_follow_txn(write, key.as_slice())?.map(|f| f.state);
        let created_at_ms = read_follow_txn(write, key.as_slice())?
            .map_or(now_ms, |f| f.created_at_ms);

        // Unlocked targets accept at once, as Mastodon does for a plain follow.
        let new_state = if target.locked {
            FollowState::Pending
        } else {
            FollowState::Active
        };
        let follow = Follow {
            follower_id: remote.id,
            followed_id: target.id,
            show_reblogs: true,
            notify: false,
            languages: None,
            state: new_state,
            created_at_ms,
            uri: Some(follow_uri.to_string()),
        };

        let mut target_stats = read_stats_txn(write, target.id)?;
        let mut remote_stats = read_stats_txn(write, remote.id)?;
        match (old_state, new_state) {
            (None, FollowState::Active) => {
                target_stats.followers_count += 1;
                remote_stats.following_count += 1;
            }
            (None, FollowState::Pending) => target_stats.follow_requests_count += 1,
            (Some(FollowState::Pending), FollowState::Active) => {
                target_stats.follow_requests_count = target_stats.follow_requests_count.saturating_sub(1);
                target_stats.followers_count += 1;
                remote_stats.following_count += 1;
            }
            _ => {}
        }

        // The same notification rules as a local follow (`RelationshipStore::follow`).
        let fan_out = if new_state == FollowState::Active {
            (old_state != Some(FollowState::Active)).then_some(NotificationType::Follow)
        } else if old_state.is_none() {
            Some(NotificationType::FollowRequest)
        } else {
            None
        };
        if let Some(kind) = fan_out {
            let job = Job::new(
                self.next_id(),
                JobKind::FanOutNotification,
                fan_out_payload(target.id, kind, Some(remote.id), None, now_ms),
                now_ms,
                now_ms,
            );
            super::jobs::insert_job(write, &job)?;
        }

        if new_state == FollowState::Active {
            let job = delivery_job(self.next_id(), &accept, now_ms);
            super::jobs::insert_job(write, &job)?;
        }

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.insert(key.as_slice(), encode(&follow)?.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed.insert(schema::pair_key(target.id.value(), remote.id.value()).as_slice(), ())?;
        }
        write_stats_txn(write, target.id, &target_stats)?;
        write_stats_txn(write, remote.id, &remote_stats)?;
        Ok(InboundDisposition::Applied)
    }

    /// Apply an inbound Accept of one of our outbound follows inside the
    /// caller's transaction. The accepting actor must be the followed account.
    fn apply_inbound_accept(
        &self,
        write: &WriteTransaction,
        remote: &Account,
        follow_uri: &str,
    ) -> Result<InboundDisposition> {
        let key = {
            let by_uri = write.open_table(FOLLOW_BY_URI)?;
            let found = by_uri.get(follow_uri)?.map(|g| g.value().to_vec());
            found
        };
        let Some(key) = key else {
            return Ok(InboundDisposition::Ignored(format!("no outbound follow {follow_uri}")));
        };
        let Some(mut follow) = read_follow_txn(write, key.as_slice())? else {
            return Ok(InboundDisposition::Ignored(format!("follow row missing for {follow_uri}")));
        };
        if follow.followed_id != remote.id {
            return Ok(InboundDisposition::Ignored(
                "accept from an actor other than the followed account".into(),
            ));
        }
        if follow.state != FollowState::Pending {
            return Ok(InboundDisposition::Ignored("follow is not pending".into()));
        }
        follow.state = FollowState::Active;

        let mut remote_stats = read_stats_txn(write, remote.id)?;
        let mut local_stats = read_stats_txn(write, follow.follower_id)?;
        remote_stats.follow_requests_count = remote_stats.follow_requests_count.saturating_sub(1);
        remote_stats.followers_count += 1;
        local_stats.following_count += 1;
        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.insert(key.as_slice(), encode(&follow)?.as_slice())?;
        }
        write_stats_txn(write, remote.id, &remote_stats)?;
        write_stats_txn(write, follow.follower_id, &local_stats)?;
        Ok(InboundDisposition::Applied)
    }

    /// Apply an inbound Reject of one of our outbound follows: the pending row
    /// and its URI index go, with the counters it moved. Only the followed
    /// account may reject, and only a pending follow can be rejected.
    fn apply_inbound_reject(
        &self,
        write: &WriteTransaction,
        remote: &Account,
        follow_uri: &str,
    ) -> Result<InboundDisposition> {
        let key = {
            let by_uri = write.open_table(FOLLOW_BY_URI)?;
            let found = by_uri.get(follow_uri)?.map(|g| g.value().to_vec());
            found
        };
        let Some(key) = key else {
            return Ok(InboundDisposition::Ignored(format!("no outbound follow {follow_uri}")));
        };
        let Some(follow) = read_follow_txn(write, key.as_slice())? else {
            return Ok(InboundDisposition::Ignored(format!("follow row missing for {follow_uri}")));
        };
        if follow.followed_id != remote.id {
            return Ok(InboundDisposition::Ignored(
                "reject from an actor other than the followed account".into(),
            ));
        }
        if follow.state != FollowState::Pending {
            return Ok(InboundDisposition::Ignored("follow is not pending".into()));
        }
        super::relationships::unfollow_txn(write, follow.follower_id, follow.followed_id)?;
        let mut by_uri = write.open_table(FOLLOW_BY_URI)?;
        by_uri.remove(follow_uri)?;
        Ok(InboundDisposition::Applied)
    }
}
