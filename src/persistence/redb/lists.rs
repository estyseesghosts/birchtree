//! List and membership persistence.
//!
//! Every mutation commits the record, its indexes, and (for membership
//! changes) any follow-up index work in ONE `redb` transaction
//! (`birchtree.md` §5). Unfollow/block cleanup is handled by
//! [`retain_membership_txn`], called from the relationship adapter inside the
//! same transaction that removes the follow.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::ListStore;
use crate::domain::{Error, Follow, FollowState, Id, List, RepliesPolicy, Result};

use super::schema::{self, LISTS, LISTS_BY_ACCOUNT, LIST_MEMBERS, MEMBERSHIPS_BY_ACCOUNT};
use super::{decode, encode, RedbStore, StoreResult};

/// Scan cap for per-account list scans (the per-account limit is 50).
const LIST_SCAN_CAP: usize = 100;
/// Scan cap for member list scans.
const MEMBER_SCAN_CAP: usize = 2_000;

/// Read one list row inside an open write transaction.
pub(super) fn read_list_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<List>> {
    let table = write.open_table(LISTS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Remove `member_id` from every list owned by `owner_id`.
///
/// Called from the follow-removal paths (unfollow, remove-from-followers,
/// block) inside their transaction: Mastodon cascades
/// `list_accounts.follow_id` on follow deletion, so a member added under a
/// follow loses membership the moment the follow goes away.
pub(super) fn retain_membership_txn(
    write: &WriteTransaction,
    owner_id: Id,
    member_id: Id,
) -> StoreResult<()> {
    let mut owned: Vec<Id> = Vec::new();
    {
        let index = write.open_table(LISTS_BY_ACCOUNT)?;
        let lo = schema::pair_key(owner_id.value(), 0);
        let hi = schema::pair_key(owner_id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
                let list_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
            owned.push(list_id);
            if owned.len() >= LIST_SCAN_CAP {
                break;
            }
        }
    }
    for list_id in owned {
        let key = schema::pair_key(list_id.value(), member_id.value());
        {
            let mut members = write.open_table(LIST_MEMBERS)?;
            members.remove(key.as_slice())?;
        }
        {
            let mut memberships = write.open_table(MEMBERSHIPS_BY_ACCOUNT)?;
            memberships.remove(key.as_slice())?;
        }
    }
    Ok(())
}

impl ListStore for RedbStore {
    fn create_list(&self, account_id: Id, title: &str, now_ms: u64) -> Result<List> {
        let write = self.begin_write()?;
        let list = List {
            id: self.next_id(),
            account_id,
            title: title.to_string(),
            replies_policy: RepliesPolicy::default(),
            exclusive: false,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(LISTS)?;
            table.insert(list.id.value(), encode(&list)?.as_slice())?;
        }
        {
            let mut table = write.open_table(LISTS_BY_ACCOUNT)?;
            table.insert(
                schema::pair_key(list.account_id.value(), list.id.value()).as_slice(),
                (),
            )?;
        }
        write.commit()?;
        Ok(list)
    }

    fn list_by_id(&self, id: Id) -> Result<Option<List>> {
        let read = self.begin_read()?;
        let table = read.open_table(LISTS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn lists_by_account(&self, account_id: Id) -> Result<Vec<List>> {
        let read = self.begin_read()?;
        let index = read.open_table(LISTS_BY_ACCOUNT)?;
        let lists = read.open_table(LISTS)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap())));
            if ids.len() >= LIST_SCAN_CAP {
                break;
            }
        }
        // Descending list id (newest first), like Mastodon's default order.
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(guard) = lists.get(id.value())? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn update_list(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        replies_policy: Option<RepliesPolicy>,
        exclusive: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<List>> {
        let _ = now_ms;
        let write = self.begin_write()?;
        let Some(mut list) = read_list_txn(&write, id)? else {
            return Ok(None);
        };
        if list.account_id != account_id {
            return Ok(None);
        }
        if let Some(title) = title {
            list.title = title;
        }
        if let Some(policy) = replies_policy {
            list.replies_policy = policy;
        }
        if let Some(exclusive) = exclusive {
            list.exclusive = exclusive;
        }
        {
            let mut table = write.open_table(LISTS)?;
            table.insert(list.id.value(), encode(&list)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(list))
    }

    fn delete_list(&self, account_id: Id, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let Some(list) = read_list_txn(&write, id)? else {
            return Ok(false);
        };
        if list.account_id != account_id {
            return Ok(false);
        }
        // Memberships: drop every member row and its reverse index entry.
        let key_lo = schema::pair_key(id.value(), 0);
        let key_hi = schema::pair_key(id.value(), u64::MAX);
        let mut members: Vec<Id> = Vec::new();
        {
            let table = write.open_table(LIST_MEMBERS)?;
            for entry in table.range(key_lo.as_slice()..=key_hi.as_slice())? {
                let (key, _value) = entry?;
                members.push(Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap())));
                if members.len() >= MEMBER_SCAN_CAP {
                    break;
                }
            }
        }
        for member in members {
            let pair = schema::pair_key(id.value(), member.value());
            let mut table = write.open_table(LIST_MEMBERS)?;
            table.remove(pair.as_slice())?;
            let mut reverse = write.open_table(MEMBERSHIPS_BY_ACCOUNT)?;
            reverse.remove(pair.as_slice())?;
        }
        {
            let mut table = write.open_table(LISTS)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(LISTS_BY_ACCOUNT)?;
            table.remove(schema::pair_key(list.account_id.value(), id.value()).as_slice())?;
        }
        write.commit()?;
        Ok(true)
    }

    fn add_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>> {
        let write = self.begin_write()?;
        let Some(list) = read_list_txn(&write, list_id)? else {
            return Err(Error::NotFound("list".into()));
        };
        let mut added = Vec::new();
        for account_id in account_ids {
            if *account_id == list.account_id {
                continue;
            }
            let pair = schema::pair_key(list_id.value(), account_id.value());
            let existing = {
                let table = write.open_table(LIST_MEMBERS)?;
                table.get(pair.as_slice())?.is_some()
            };
            if existing {
                continue;
            }
            // Membership requires an active follow (or pending request) from
            // the list owner, mirroring `ListAccount#validate_relationship`.
            let follows = write.open_table(super::schema::FOLLOWS)?;
            let follow: Option<Follow> = match follows.get(
                schema::pair_key(list.account_id.value(), account_id.value()).as_slice(),
            )? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            };
            let permitted = follow
                .as_ref()
                .is_some_and(|f| matches!(f.state, FollowState::Active | FollowState::Pending));
            if !permitted {
                return Err(Error::InvalidInput(
                    "Account must be following".into(),
                ));
            }
            {
                let mut table = write.open_table(LIST_MEMBERS)?;
                table.insert(pair.as_slice(), ())?;
            }
            {
                let mut table = write.open_table(MEMBERSHIPS_BY_ACCOUNT)?;
                table.insert(pair.as_slice(), ())?;
            }
            added.push(*account_id);
        }
        write.commit()?;
        Ok(added)
    }

    fn remove_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>> {
        let write = self.begin_write()?;
        if read_list_txn(&write, list_id)?.is_none() {
            return Err(Error::NotFound("list".into()));
        }
        let mut removed = Vec::new();
        for account_id in account_ids {
            let pair = schema::pair_key(list_id.value(), account_id.value());
            let existed = {
                let mut table = write.open_table(LIST_MEMBERS)?;
                table.remove(pair.as_slice())?.is_some()
            };
            {
                let mut table = write.open_table(MEMBERSHIPS_BY_ACCOUNT)?;
                table.remove(pair.as_slice())?;
            }
            if existed {
                removed.push(*account_id);
            }
        }
        write.commit()?;
        Ok(removed)
    }

    fn list_members(&self, list_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(LIST_MEMBERS)?;
        let lo = schema::pair_key(list_id.value(), 0);
        let hi = schema::pair_key(list_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap())));
            if ids.len() >= MEMBER_SCAN_CAP {
                break;
            }
        }
        // Descending member id (newest first), cursor-bounded.
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids
            .into_iter()
            .filter(|id| max_id.is_none_or(|max| *id < max))
            .take(limit)
            .collect())
    }

    fn all_list_members(&self, list_id: Id, cap: usize) -> Result<Vec<Id>> {
        let read = self.begin_read()?;
        let table = read.open_table(LIST_MEMBERS)?;
        let lo = schema::pair_key(list_id.value(), 0);
        let hi = schema::pair_key(list_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap())));
            if ids.len() >= cap {
                break;
            }
        }
        self.note_scanned(ids.len());
        Ok(ids)
    }
}
