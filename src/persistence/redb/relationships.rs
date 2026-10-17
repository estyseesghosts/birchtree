//! Relationship persistence: follows and follow requests, blocks, mutes,
//! private account notes, domain blocks, endorsements and the pairwise
//! relationship aggregate.
//!
//! Every mutation commits its record, its indexes, both sides' counters and
//! any follow-up notification job in ONE `redb` transaction
//! (`birchtree.md` §5); the notification itself is fan-out work, created
//! after commit by the `FanOutNotification` job.

use redb::{ReadableDatabase, ReadableTable, WriteTransaction};

use crate::application::notifications::fan_out_payload;
use crate::application::ports::{
    FollowOptions, FollowOutcome, PairState, Relationship, RelationshipStore,
};
use crate::domain::{
    AccountNote, AccountStats, Block, DomainBlock, Endorsement, Error, Follow, FollowState, Id,
    Job, JobKind, Mute, NotificationType, Result,
};

use super::schema::{
    self, ACCOUNT_NOTES, ACCOUNT_STATS, BLOCKS, BLOCKS_BY_BLOCKED, DOMAIN_BLOCKS, ENDORSEMENTS,
    ENDORSEMENTS_BY_ENDORSED, FOLLOWS, FOLLOWS_BY_FOLLOWED, MUTES, MUTES_BY_MUTED,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Scan cap for `follow_requests`: index entries examined per call, so a
/// locked account with a pathological request backlog cannot pin a long scan.
const FOLLOW_REQUEST_SCAN_CAP: usize = 1_000;

/// Scan cap for `followers`/`following`: index entries examined per call.
const FOLLOWER_SCAN_CAP: usize = 2_000;

/// Scan cap for `endorsements`: index entries examined per call.
const ENDORSEMENT_SCAN_CAP: usize = 1_000;

/// Mastodon's `endorse` precondition (`pin_errors.following`), wrapped the
/// way `ActiveRecord::RecordInvalid` surfaces it.
const ENDORSE_NEEDS_FOLLOW: &str =
    "Validation failed: You must be already following the person you want to endorse";

/// Decode the JSON row stored at `key`, if present. Accepts handles opened on
/// either a read or a write transaction.
fn json_at<T, R>(table: &T, key: &[u8]) -> StoreResult<Option<R>>
where
    T: ReadableTable<&'static [u8], &'static [u8]> + ?Sized,
    R: serde::de::DeserializeOwned,
{
    match table.get(key)? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Read one follow row inside an existing (write) transaction.
fn read_follow_txn(write: &WriteTransaction, key: &[u8]) -> StoreResult<Option<Follow>> {
    let follows = write.open_table(FOLLOWS)?;
    json_at(&follows, key)
}

/// Read one account's counters inside an existing transaction. A missing row
/// is [`AccountStats::default`]: counters accumulate from zero, never from a
/// created row.
fn read_stats_txn(write: &WriteTransaction, account_id: Id) -> StoreResult<AccountStats> {
    let stats = write.open_table(ACCOUNT_STATS)?;
    match stats.get(account_id.value())? {
        Some(guard) => Ok(decode(guard.value())?),
        None => Ok(AccountStats::default()),
    }
}

/// Persist one account's counters inside the same transaction that moved
/// them.
fn write_stats_txn(
    write: &WriteTransaction,
    account_id: Id,
    stats: &AccountStats,
) -> StoreResult<()> {
    let mut table = write.open_table(ACCOUNT_STATS)?;
    table.insert(account_id.value(), encode(stats)?.as_slice())?;
    Ok(())
}

/// Remove an endorsement and its reverse index inside an existing
/// transaction (no-op when absent; a follow's removal takes matching
/// endorsements with it, per Mastodon `AccountPin` cleanup).
fn remove_endorsement_txn(
    write: &WriteTransaction,
    account_id: Id,
    endorsed_id: Id,
) -> StoreResult<()> {
    let pair = schema::pair_key(account_id.value(), endorsed_id.value());
    {
        let mut endorsements = write.open_table(ENDORSEMENTS)?;
        endorsements.remove(pair.as_slice())?;
    }
    {
        let mut by_endorsed = write.open_table(ENDORSEMENTS_BY_ENDORSED)?;
        by_endorsed.remove(
            schema::pair_key(endorsed_id.value(), account_id.value()).as_slice(),
        )?;
    }
    Ok(())
}

impl RelationshipStore for RedbStore {
    fn follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        opts: FollowOptions,
        request: bool,
        now_ms: u64,
    ) -> Result<FollowOutcome> {
        if follower_id == followed_id {
            return Err(Error::Forbidden(
                "Following your own account is not allowed".into(),
            ));
        }

        // ONE transaction: row, reverse index, both sides' counters and the
        // notification job commit (or abort) together.
        let write = self.db().begin_write()?;
        let key = schema::pair_key(follower_id.value(), followed_id.value());
        let existing = read_follow_txn(&write, key.as_slice())?;
        let old_state = existing.as_ref().map(|f| f.state);

        // A `request` against anything but an accepted follow stays pending
        // (locked accounts); re-following an active relation only refreshes
        // its options and keeps the row active.
        let new_state = match (request, old_state) {
            (true, Some(FollowState::Active)) => FollowState::Active,
            (true, _) => FollowState::Pending,
            (false, _) => FollowState::Active,
        };

        let follow = Follow {
            follower_id,
            followed_id,
            show_reblogs: opts.show_reblogs,
            notify: opts.notify,
            state: new_state,
            created_at_ms: existing.as_ref().map_or(now_ms, |f| f.created_at_ms),
            uri: None,
        };

        // Counter moves, keyed by the (old, new) state transition.
        let mut follower_stats = read_stats_txn(&write, follower_id)?;
        let mut followed_stats = read_stats_txn(&write, followed_id)?;
        match (old_state, new_state) {
            (None, FollowState::Active) => {
                followed_stats.followers_count += 1;
                follower_stats.following_count += 1;
            }
            (None, FollowState::Pending) => {
                followed_stats.follow_requests_count += 1;
            }
            (Some(FollowState::Pending), FollowState::Active) => {
                followed_stats.follow_requests_count =
                    followed_stats.follow_requests_count.saturating_sub(1);
                followed_stats.followers_count += 1;
                follower_stats.following_count += 1;
            }
            // Options-only updates (re-follow / re-request) move no counters;
            // `request` never demotes an accepted follow, so (Active, Pending)
            // cannot occur.
            (Some(FollowState::Active), FollowState::Active)
            | (Some(FollowState::Pending), FollowState::Pending)
            | (Some(FollowState::Active), FollowState::Pending) => {}
        }

        // The fan-out job commits atomically with the state change; the
        // notification record is created after commit, by the worker.
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
                fan_out_payload(followed_id, kind, Some(follower_id), None, now_ms),
                now_ms,
                now_ms,
            );
            super::jobs::insert_job(&write, &job)?;
        }

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.insert(key.as_slice(), encode(&follow)?.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed
                .insert(schema::pair_key(followed_id.value(), follower_id.value()).as_slice(), ())?;
        }
        write_stats_txn(&write, follower_id, &follower_stats)?;
        write_stats_txn(&write, followed_id, &followed_stats)?;
        write.commit()?;

        Ok(if new_state == FollowState::Active {
            FollowOutcome::Following(follow)
        } else {
            FollowOutcome::Requested(follow)
        })
    }

    fn unfollow(&self, follower_id: Id, followed_id: Id, now_ms: u64) -> Result<Option<Follow>> {
        let _ = now_ms; // Removal records no transition timestamp and no job.
        let write = self.db().begin_write()?;
        let key = schema::pair_key(follower_id.value(), followed_id.value());
        let Some(existing) = read_follow_txn(&write, key.as_slice())? else {
            return Ok(None);
        };

        let mut follower_stats = read_stats_txn(&write, follower_id)?;
        let mut followed_stats = read_stats_txn(&write, followed_id)?;
        match existing.state {
            FollowState::Active => {
                followed_stats.followers_count = followed_stats.followers_count.saturating_sub(1);
                follower_stats.following_count = follower_stats.following_count.saturating_sub(1);
            }
            FollowState::Pending => {
                followed_stats.follow_requests_count =
                    followed_stats.follow_requests_count.saturating_sub(1);
            }
        }

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.remove(key.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed
                .remove(schema::pair_key(followed_id.value(), follower_id.value()).as_slice())?;
        }
        // Mastodon destroys matching AccountPins with the follow.
        remove_endorsement_txn(&write, follower_id, followed_id)?;
        write_stats_txn(&write, follower_id, &follower_stats)?;
        write_stats_txn(&write, followed_id, &followed_stats)?;
        write.commit()?;
        Ok(Some(existing))
    }

    fn remove_follower(&self, account_id: Id, follower_id: Id, now_ms: u64) -> Result<Option<Follow>> {
        let _ = now_ms; // Removal records no transition timestamp and no job.
        let write = self.db().begin_write()?;
        let key = schema::pair_key(follower_id.value(), account_id.value());
        let Some(existing) = read_follow_txn(&write, key.as_slice())? else {
            return Ok(None);
        };

        let mut account_stats = read_stats_txn(&write, account_id)?;
        let mut follower_stats = read_stats_txn(&write, follower_id)?;
        match existing.state {
            FollowState::Active => {
                account_stats.followers_count = account_stats.followers_count.saturating_sub(1);
                follower_stats.following_count = follower_stats.following_count.saturating_sub(1);
            }
            FollowState::Pending => {
                account_stats.follow_requests_count =
                    account_stats.follow_requests_count.saturating_sub(1);
            }
        }

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.remove(key.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed.remove(schema::pair_key(account_id.value(), follower_id.value()).as_slice())?;
        }
        // Destroying a follow destroys its matching endorsement.
        remove_endorsement_txn(&write, follower_id, account_id)?;
        write_stats_txn(&write, account_id, &account_stats)?;
        write_stats_txn(&write, follower_id, &follower_stats)?;
        write.commit()?;
        Ok(Some(existing))
    }

    fn follow_requests(
        &self,
        target_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Follow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.db().begin_read()?;
        let lower = schema::pair_key(target_id.value(), 0);
        let upper = schema::pair_key(target_id.value(), u64::MAX);

        // Keys ascend by follower id; a bounded window reversed gives the
        // newest requests first.
        let mut follower_ids: Vec<Id> = Vec::new();
        {
            let by_followed = read.open_table(FOLLOWS_BY_FOLLOWED)?;
            for entry in by_followed.range(lower.as_slice()..=upper.as_slice())? {
                let (key, _value) = entry?;
                follower_ids.push(Id::new(u64::from_be_bytes(
                    key.value()[8..].try_into().unwrap(),
                )));
                if follower_ids.len() >= FOLLOW_REQUEST_SCAN_CAP {
                    break;
                }
            }
        }
        follower_ids.reverse();

        let mut out: Vec<Follow> = Vec::new();
        let follows = read.open_table(FOLLOWS)?;
        for follower_id in follower_ids {
            if out.len() >= limit {
                break;
            }
            if max_id.is_some_and(|max| follower_id >= max) {
                continue;
            }
            let Some(follow) = json_at::<_, Follow>(
                &follows,
                schema::pair_key(follower_id.value(), target_id.value()).as_slice(),
            )?
            else {
                continue;
            };
            if follow.state != FollowState::Pending {
                continue;
            }
            out.push(follow);
        }
        Ok(out)
    }

    fn authorize_follow_request(
        &self,
        target_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Follow> {
        let write = self.db().begin_write()?;
        let key = schema::pair_key(follower_id.value(), target_id.value());
        let Some(mut follow) = read_follow_txn(&write, key.as_slice())? else {
            return Err(Error::NotFound("follow request".into()));
        };
        if follow.state != FollowState::Pending {
            return Err(Error::NotFound("follow request".into()));
        }
        follow.state = FollowState::Active; // `created_at_ms` keeps the ask's time.

        let mut target_stats = read_stats_txn(&write, target_id)?;
        let mut follower_stats = read_stats_txn(&write, follower_id)?;
        target_stats.follow_requests_count = target_stats.follow_requests_count.saturating_sub(1);
        target_stats.followers_count += 1;
        follower_stats.following_count += 1;

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.insert(key.as_slice(), encode(&follow)?.as_slice())?;
        }
        // The requester learns the target accepted: recipient = follower.
        let job = Job::new(
            self.next_id(),
            JobKind::FanOutNotification,
            fan_out_payload(
                follower_id,
                NotificationType::Follow,
                Some(target_id),
                None,
                now_ms,
            ),
            now_ms,
            now_ms,
        );
        super::jobs::insert_job(&write, &job)?;
        write_stats_txn(&write, target_id, &target_stats)?;
        write_stats_txn(&write, follower_id, &follower_stats)?;
        write.commit()?;
        Ok(follow)
    }

    fn reject_follow_request(
        &self,
        target_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Option<Follow>> {
        let _ = now_ms; // A dropped request schedules no job.
        let write = self.db().begin_write()?;
        let key = schema::pair_key(follower_id.value(), target_id.value());
        let Some(existing) = read_follow_txn(&write, key.as_slice())? else {
            return Ok(None);
        };
        if existing.state != FollowState::Pending {
            return Ok(None);
        }

        let mut target_stats = read_stats_txn(&write, target_id)?;
        target_stats.follow_requests_count = target_stats.follow_requests_count.saturating_sub(1);

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.remove(key.as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed.remove(schema::pair_key(target_id.value(), follower_id.value()).as_slice())?;
        }
        write_stats_txn(&write, target_id, &target_stats)?;
        write.commit()?;
        Ok(Some(existing))
    }

    fn block(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>> {
        if account_id == target_id {
            return Ok(None);
        }
        // ONE transaction: both follows (with counters), both endorsements and
        // the block row commit together.
        let write = self.db().begin_write()?;
        let block_key = schema::pair_key(account_id.value(), target_id.value());
        let existing: Option<Block> = {
            let blocks = write.open_table(BLOCKS)?;
            json_at(&blocks, block_key.as_slice())?
        };
        if existing.is_some() {
            return Ok(None); // Already blocked: no-op.
        }

        let outgoing = read_follow_txn(&write, block_key.as_slice())?;
        let inbound =
            read_follow_txn(&write, schema::pair_key(target_id.value(), account_id.value()).as_slice())?;

        let mut account_stats = read_stats_txn(&write, account_id)?;
        let mut target_stats = read_stats_txn(&write, target_id)?;
        if let Some(follow) = &outgoing {
            match follow.state {
                FollowState::Active => {
                    account_stats.following_count = account_stats.following_count.saturating_sub(1);
                    target_stats.followers_count = target_stats.followers_count.saturating_sub(1);
                }
                FollowState::Pending => {
                    target_stats.follow_requests_count =
                        target_stats.follow_requests_count.saturating_sub(1);
                }
            }
        }
        if let Some(follow) = &inbound {
            match follow.state {
                FollowState::Active => {
                    target_stats.following_count = target_stats.following_count.saturating_sub(1);
                    account_stats.followers_count = account_stats.followers_count.saturating_sub(1);
                }
                FollowState::Pending => {
                    account_stats.follow_requests_count =
                        account_stats.follow_requests_count.saturating_sub(1);
                }
            }
        }

        {
            let mut follows = write.open_table(FOLLOWS)?;
            follows.remove(block_key.as_slice())?;
            follows.remove(schema::pair_key(target_id.value(), account_id.value()).as_slice())?;
        }
        {
            let mut by_followed = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            by_followed
                .remove(schema::pair_key(target_id.value(), account_id.value()).as_slice())?;
            by_followed
                .remove(schema::pair_key(account_id.value(), target_id.value()).as_slice())?;
        }
        // Blocking severs endorsements in both directions.
        remove_endorsement_txn(&write, account_id, target_id)?;
        remove_endorsement_txn(&write, target_id, account_id)?;
        write_stats_txn(&write, account_id, &account_stats)?;
        write_stats_txn(&write, target_id, &target_stats)?;

        let block = Block {
            blocker_id: account_id,
            blocked_id: target_id,
            created_at_ms: now_ms,
        };
        {
            let mut blocks = write.open_table(BLOCKS)?;
            blocks.insert(block_key.as_slice(), encode(&block)?.as_slice())?;
        }
        {
            let mut by_blocked = write.open_table(BLOCKS_BY_BLOCKED)?;
            by_blocked
                .insert(schema::pair_key(target_id.value(), account_id.value()).as_slice(), ())?;
        }
        write.commit()?;
        Ok(Some(block))
    }

    fn unblock(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>> {
        let _ = now_ms; // A dropped block records no timestamp.
        let write = self.db().begin_write()?;
        let block_key = schema::pair_key(account_id.value(), target_id.value());
        let existing: Option<Block> = {
            let blocks = write.open_table(BLOCKS)?;
            json_at(&blocks, block_key.as_slice())?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        {
            let mut blocks = write.open_table(BLOCKS)?;
            blocks.remove(block_key.as_slice())?;
        }
        {
            let mut by_blocked = write.open_table(BLOCKS_BY_BLOCKED)?;
            by_blocked
                .remove(schema::pair_key(target_id.value(), account_id.value()).as_slice())?;
        }
        write.commit()?;
        Ok(Some(existing))
    }

    fn mute(
        &self,
        account_id: Id,
        target_id: Id,
        duration_seconds: u64,
        hide_notifications: bool,
        now_ms: u64,
    ) -> Result<Mute> {
        // 0 = permanent; a duration becomes an absolute expiry.
        let expire_at_ms = if duration_seconds > 0 {
            Some(now_ms.saturating_add(duration_seconds.saturating_mul(1_000)))
        } else {
            None
        };
        let mute = Mute {
            muter_id: account_id,
            muted_id: target_id,
            expire_at_ms,
            notify: hide_notifications,
            created_at_ms: now_ms,
        };
        let write = self.db().begin_write()?;
        {
            let mut mutes = write.open_table(MUTES)?;
            mutes.insert(
                schema::pair_key(account_id.value(), target_id.value()).as_slice(),
                encode(&mute)?.as_slice(),
            )?;
        }
        {
            let mut by_muted = write.open_table(MUTES_BY_MUTED)?;
            by_muted.insert(schema::pair_key(target_id.value(), account_id.value()).as_slice(), ())?;
        }
        write.commit()?;
        Ok(mute)
    }

    fn unmute(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Mute>> {
        let _ = now_ms; // A dropped mute records no timestamp.
        let write = self.db().begin_write()?;
        let key = schema::pair_key(account_id.value(), target_id.value());
        let existing: Option<Mute> = {
            let mutes = write.open_table(MUTES)?;
            json_at(&mutes, key.as_slice())?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        {
            let mut mutes = write.open_table(MUTES)?;
            mutes.remove(key.as_slice())?;
        }
        {
            let mut by_muted = write.open_table(MUTES_BY_MUTED)?;
            by_muted.remove(schema::pair_key(target_id.value(), account_id.value()).as_slice())?;
        }
        write.commit()?;
        Ok(Some(existing))
    }

    fn set_account_note(
        &self,
        viewer_id: Id,
        target_id: Id,
        comment: &str,
        now_ms: u64,
    ) -> Result<Option<AccountNote>> {
        let write = self.db().begin_write()?;
        let key = schema::pair_key(viewer_id.value(), target_id.value());
        if comment.is_empty() {
            // An empty comment clears the note (Mastodon deletes the row).
            {
                let mut notes = write.open_table(ACCOUNT_NOTES)?;
                notes.remove(key.as_slice())?;
            }
            write.commit()?;
            return Ok(None);
        }
        let note = AccountNote {
            viewer_id,
            target_id,
            comment: comment.to_string(),
            updated_at_ms: now_ms,
        };
        {
            let mut notes = write.open_table(ACCOUNT_NOTES)?;
            notes.insert(key.as_slice(), encode(&note)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(note))
    }

    fn domain_block(&self, account_id: Id, domain: &str, now_ms: u64) -> Result<DomainBlock> {
        if domain.is_empty() {
            return Err(Error::InvalidInput("Domain cannot be blank".into()));
        }
        let write = self.db().begin_write()?;
        let key = schema::domain_block_key(account_id.value(), domain);
        let existing: Option<DomainBlock> = {
            let blocks = write.open_table(DOMAIN_BLOCKS)?;
            json_at(&blocks, key.as_slice())?
        };
        // Re-blocking keeps the original creation time.
        let block = DomainBlock {
            account_id,
            domain: domain.to_string(),
            created_at_ms: existing.map_or(now_ms, |b| b.created_at_ms),
        };
        {
            let mut blocks = write.open_table(DOMAIN_BLOCKS)?;
            blocks.insert(key.as_slice(), encode(&block)?.as_slice())?;
        }
        write.commit()?;
        Ok(block)
    }

    fn unblock_domain(&self, account_id: Id, domain: &str) -> Result<Option<DomainBlock>> {
        let write = self.db().begin_write()?;
        let key = schema::domain_block_key(account_id.value(), domain);
        let existing: Option<DomainBlock> = {
            let blocks = write.open_table(DOMAIN_BLOCKS)?;
            json_at(&blocks, key.as_slice())?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        {
            let mut blocks = write.open_table(DOMAIN_BLOCKS)?;
            blocks.remove(key.as_slice())?;
        }
        write.commit()?;
        Ok(Some(existing))
    }

    fn domain_blocks(
        &self,
        account_id: Id,
        max_domain: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DomainBlock>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.db().begin_read()?;
        let blocks = read.open_table(DOMAIN_BLOCKS)?;
        let prefix = schema::domain_block_key(account_id.value(), "");
        // Keys ascend by domain. A `max_domain` cursor starts strictly after
        // its own key: appending 0x00 (domains cannot contain NUL) skips
        // `domain == max_domain` while staying below every larger domain.
        let start = match max_domain {
            Some(domain) => {
                let mut start = schema::domain_block_key(account_id.value(), domain);
                start.push(0x00);
                start
            }
            None => prefix.clone(),
        };

        // Ascending by domain: the endpoint exposes domain cursors.
        let mut out: Vec<DomainBlock> = Vec::new();
        for entry in blocks.range(start.as_slice()..)? {
            let (key, value) = entry?;
            if !key.value().starts_with(&prefix) {
                break; // Past this account's block list.
            }
            out.push(decode(value.value())?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn endorse(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Endorsement> {
        let write = self.db().begin_write()?;
        let pair = schema::pair_key(account_id.value(), target_id.value());

        // Mastodon pins only accounts you actively follow.
        let follow: Option<Follow> = {
            let follows = write.open_table(FOLLOWS)?;
            json_at(&follows, pair.as_slice())?
        };
        if !follow.is_some_and(|f| f.state == FollowState::Active) {
            return Err(Error::InvalidInput(ENDORSE_NEEDS_FOLLOW.into()));
        }

        let existing: Option<Endorsement> = {
            let endorsements = write.open_table(ENDORSEMENTS)?;
            json_at(&endorsements, pair.as_slice())?
        };
        if let Some(existing) = existing {
            return Ok(existing); // Idempotent: already pinned.
        }

        let endorsement = Endorsement {
            account_id,
            endorsed_id: target_id,
            created_at_ms: now_ms,
        };
        {
            let mut endorsements = write.open_table(ENDORSEMENTS)?;
            endorsements.insert(pair.as_slice(), encode(&endorsement)?.as_slice())?;
        }
        {
            let mut by_endorsed = write.open_table(ENDORSEMENTS_BY_ENDORSED)?;
            by_endorsed
                .insert(schema::pair_key(target_id.value(), account_id.value()).as_slice(), ())?;
        }
        write.commit()?;
        Ok(endorsement)
    }

    fn unendorse(&self, account_id: Id, target_id: Id) -> Result<Option<Endorsement>> {
        let write = self.db().begin_write()?;
        let pair = schema::pair_key(account_id.value(), target_id.value());
        let existing: Option<Endorsement> = {
            let endorsements = write.open_table(ENDORSEMENTS)?;
            json_at(&endorsements, pair.as_slice())?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        remove_endorsement_txn(&write, account_id, target_id)?;
        write.commit()?;
        Ok(Some(existing))
    }

    fn endorsements(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.db().begin_read()?;
        let lower = schema::pair_key(account_id.value(), 0);
        let upper = schema::pair_key(account_id.value(), u64::MAX);

        // Keys ascend by endorsed id; a bounded window reversed gives the
        // newest endorsements first.
        let mut endorsed: Vec<Id> = Vec::new();
        {
            let endorsements = read.open_table(ENDORSEMENTS)?;
            for entry in endorsements.range(lower.as_slice()..=upper.as_slice())? {
                let (key, _value) = entry?;
                endorsed.push(Id::new(u64::from_be_bytes(
                    key.value()[8..].try_into().unwrap(),
                )));
                if endorsed.len() >= ENDORSEMENT_SCAN_CAP {
                    break;
                }
            }
        }
        endorsed.reverse();

        Ok(endorsed
            .into_iter()
            .filter(|id| max_id.is_none_or(|max| *id < max))
            .take(limit)
            .collect())
    }

    fn relationships(
        &self,
        viewer_id: Id,
        targets: &[(Id, String)],
        now_ms: u64,
    ) -> Result<Vec<Relationship>> {
        let read = self.db().begin_read()?;
        let follows = read.open_table(FOLLOWS)?;
        let blocks = read.open_table(BLOCKS)?;
        let mutes = read.open_table(MUTES)?;
        let domain_blocks = read.open_table(DOMAIN_BLOCKS)?;
        let endorsements = read.open_table(ENDORSEMENTS)?;
        let notes = read.open_table(ACCOUNT_NOTES)?;

        let mut out: Vec<Relationship> = Vec::with_capacity(targets.len());
        for (target_id, domain) in targets {
            // Every row is optional; a missing row means "no such relation".
            let follow_out: Option<Follow> = json_at(
                &follows,
                schema::pair_key(viewer_id.value(), target_id.value()).as_slice(),
            )?;
            let follow_in: Option<Follow> = json_at(
                &follows,
                schema::pair_key(target_id.value(), viewer_id.value()).as_slice(),
            )?;
            let blocking: Option<Block> = json_at(
                &blocks,
                schema::pair_key(viewer_id.value(), target_id.value()).as_slice(),
            )?;
            let blocked_by: Option<Block> = json_at(
                &blocks,
                schema::pair_key(target_id.value(), viewer_id.value()).as_slice(),
            )?;
            let mute: Option<Mute> = json_at(
                &mutes,
                schema::pair_key(viewer_id.value(), target_id.value()).as_slice(),
            )?;
            let domain_key = schema::domain_block_key(viewer_id.value(), domain);
            let domain_blocking: Option<DomainBlock> =
                json_at(&domain_blocks, domain_key.as_slice())?;
            let endorsed: Option<Endorsement> = json_at(
                &endorsements,
                schema::pair_key(viewer_id.value(), target_id.value()).as_slice(),
            )?;
            let note: Option<AccountNote> = json_at(
                &notes,
                schema::pair_key(viewer_id.value(), target_id.value()).as_slice(),
            )?;

            let active_out = follow_out.as_ref().is_some_and(|f| f.state == FollowState::Active);
            let pending_out = follow_out.as_ref().is_some_and(|f| f.state == FollowState::Pending);
            let active_in = follow_in.as_ref().is_some_and(|f| f.state == FollowState::Active);
            let pending_in = follow_in.as_ref().is_some_and(|f| f.state == FollowState::Pending);
            let mute_active = mute.as_ref().is_some_and(|m| m.is_active_at(now_ms));

            out.push(Relationship {
                target_id: *target_id,
                following: active_out,
                // Per Mastodon, `showing_reblogs`/`notifying` read the
                // following row with fallback to the requested row; with NO
                // row at all the serializer answers `false`.
                show_reblogs: follow_out.as_ref().map_or(false, |f| f.show_reblogs),
                notifying: follow_out.as_ref().is_some_and(|f| f.notify),
                followed_by: active_in,
                blocking: blocking.is_some(),
                blocked_by: blocked_by.is_some(),
                muting: mute_active,
                muting_notifications: mute
                    .as_ref()
                    .is_some_and(|m| m.hides_notifications_at(now_ms)),
                muting_expires_at_ms: mute.as_ref().and_then(|m| m.expire_at_ms),
                requested: pending_out,
                requested_by: pending_in,
                domain_blocking: domain_blocking.is_some(),
                endorsed: endorsed.is_some(),
                note: note.map(|n| n.comment).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    fn pair_state(&self, a: Id, b: Id, now_ms: u64) -> Result<PairState> {
        let read = self.db().begin_read()?;
        let blocks = read.open_table(BLOCKS)?;
        let follows = read.open_table(FOLLOWS)?;
        let mutes = read.open_table(MUTES)?;

        let blocking: Option<Block> =
            json_at(&blocks, schema::pair_key(a.value(), b.value()).as_slice())?;
        let blocked_by: Option<Block> =
            json_at(&blocks, schema::pair_key(b.value(), a.value()).as_slice())?;
        let following: Option<Follow> =
            json_at(&follows, schema::pair_key(a.value(), b.value()).as_slice())?;
        let followed_by: Option<Follow> =
            json_at(&follows, schema::pair_key(b.value(), a.value()).as_slice())?;
        let mute: Option<Mute> =
            json_at(&mutes, schema::pair_key(a.value(), b.value()).as_slice())?;

        Ok(PairState {
            blocking: blocking.is_some(),
            blocked_by: blocked_by.is_some(),
            following: following.as_ref().is_some_and(|f| f.state == FollowState::Active),
            followed_by: followed_by.as_ref().is_some_and(|f| f.state == FollowState::Active),
            muting: mute.as_ref().is_some_and(|m| m.is_active_at(now_ms)),
        })
    }

    fn follow_of(&self, follower_id: Id, followed_id: Id) -> Result<Option<Follow>> {
        let read = self.db().begin_read()?;
        let follows = read.open_table(FOLLOWS)?;
        Ok(json_at(
            &follows,
            schema::pair_key(follower_id.value(), followed_id.value()).as_slice(),
        )?)
    }

    fn followers(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.db().begin_read()?;
        let lower = schema::pair_key(account_id.value(), 0);
        let upper = schema::pair_key(account_id.value(), u64::MAX);

        // Keys ascend by follower id; a bounded window reversed gives the
        // newest followers first.
        let mut follower_ids: Vec<Id> = Vec::new();
        {
            let by_followed = read.open_table(FOLLOWS_BY_FOLLOWED)?;
            for entry in by_followed.range(lower.as_slice()..=upper.as_slice())? {
                let (key, _value) = entry?;
                follower_ids.push(Id::new(u64::from_be_bytes(
                    key.value()[8..].try_into().unwrap(),
                )));
                if follower_ids.len() >= FOLLOWER_SCAN_CAP {
                    break;
                }
            }
        }
        follower_ids.reverse();

        let mut out: Vec<Id> = Vec::new();
        let follows = read.open_table(FOLLOWS)?;
        for follower_id in follower_ids {
            if out.len() >= limit {
                break;
            }
            if max_id.is_some_and(|max| follower_id >= max) {
                continue;
            }
            let Some(follow) = json_at::<_, Follow>(
                &follows,
                schema::pair_key(follower_id.value(), account_id.value()).as_slice(),
            )?
            else {
                continue;
            };
            if follow.state != FollowState::Active {
                continue;
            }
            out.push(follower_id);
        }
        Ok(out)
    }

    fn following(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.db().begin_read()?;
        let lower = schema::pair_key(account_id.value(), 0);
        let upper = schema::pair_key(account_id.value(), u64::MAX);

        // Mirror of `followers`: FOLLOWS keys are (follower || followed), so
        // this prefix scans account's outgoing follows, ascending by followed
        // id; a bounded window reversed gives the newest first.
        let mut followed_ids: Vec<Id> = Vec::new();
        {
            let follows = read.open_table(FOLLOWS)?;
            for entry in follows.range(lower.as_slice()..=upper.as_slice())? {
                let (key, _value) = entry?;
                followed_ids.push(Id::new(u64::from_be_bytes(
                    key.value()[8..].try_into().unwrap(),
                )));
                if followed_ids.len() >= FOLLOWER_SCAN_CAP {
                    break;
                }
            }
        }
        followed_ids.reverse();

        let mut out: Vec<Id> = Vec::new();
        let follows = read.open_table(FOLLOWS)?;
        for followed_id in followed_ids {
            if out.len() >= limit {
                break;
            }
            if max_id.is_some_and(|max| followed_id >= max) {
                continue;
            }
            let Some(follow) = json_at::<_, Follow>(
                &follows,
                schema::pair_key(account_id.value(), followed_id.value()).as_slice(),
            )?
            else {
                continue;
            };
            if follow.state != FollowState::Active {
                continue;
            }
            out.push(followed_id);
        }
        Ok(out)
    }

    fn account_stats(&self, account_id: Id) -> Result<AccountStats> {
        let read = self.db().begin_read()?;
        let stats = read.open_table(ACCOUNT_STATS)?;
        match stats.get(account_id.value())? {
            Some(guard) => Ok(decode(guard.value())?),
            None => Ok(AccountStats::default()),
        }
    }
}
