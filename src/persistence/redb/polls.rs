//! Poll and ballot persistence.
//!
//! A poll's options and tallies are embedded in one `POLLS` row, so a vote is
//! a single-row update inside ONE transaction that also commits the ballot,
//! its indexes and the vote-notification job (see `birchtree.md` §5).

use std::collections::HashMap;

use redb::{ReadableTable, WriteTransaction};

use crate::application::notifications::fan_out_payload;
use crate::application::ports::{NewPollSpec, PollStore, VoteRejection};
use crate::domain::{Id, Job, JobKind, NotificationType, Poll, PollOption, PollVote, Result};

use super::schema::{self, POLL_BY_STATUS, POLL_VOTES, POLLS};
use super::{decode, encode, RedbStore, StoreError, StoreResult};

/// Read one poll row inside an existing transaction.
fn read_poll_txn(write: &WriteTransaction, poll_id: Id) -> StoreResult<Option<Poll>> {
    let polls = write.open_table(POLLS)?;
    match polls.get(poll_id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// The `PollStore::vote_poll` port returns `VoteRejection`, which has no
/// storage-failure variant (the contract is frozen), so infrastructure and
/// codec failures are logged and surfaced as `Expired`: a poll the store
/// cannot read is one the use case cannot take a vote for either.
fn vote_err<E: Into<StoreError>>(err: E) -> VoteRejection {
    let err = err.into();
    tracing::error!(error = %err, "poll vote storage failure");
    VoteRejection::Expired
}

/// Insert a new poll and its status index inside the status-creation
/// transaction (`statuses.rs` calls this); returns the stored record.
pub(super) fn insert_new_poll_txn(
    write: &WriteTransaction,
    poll_id: Id,
    status_id: Id,
    account_id: Id,
    spec: &NewPollSpec,
) -> StoreResult<Poll> {
    let poll = Poll {
        id: poll_id,
        status_id,
        account_id,
        multiple: spec.multiple,
        hide_totals: spec.hide_totals,
        expires_at_ms: Some(spec.expires_at_ms),
        expired: false,
        options: spec
            .options
            .iter()
            .map(|title| PollOption {
                title: title.clone(),
                votes_count: 0,
            })
            .collect(),
        voters_count: 0,
        votes_count: 0,
    };
    super::statuses::ensure_id_unused(write, POLLS, poll_id)?;
    {
        let mut polls = write.open_table(POLLS)?;
        polls.insert(poll_id.value(), encode(&poll)?.as_slice())?;
    }
    {
        let mut by_status = write.open_table(POLL_BY_STATUS)?;
        by_status.insert(status_id.value(), poll_id.value())?;
    }
    Ok(poll)
}

/// Commit the `PollExpired` job into the status-creation transaction
/// (`statuses.rs` calls this); the job becomes claimable at the poll's expiry.
pub(super) fn schedule_expiry_job(
    write: &WriteTransaction,
    poll_id: Id,
    expires_at_ms: u64,
    now_ms: u64,
) -> StoreResult<()> {
    // A poll schedules at most one expiry job, and its generator-produced id
    // is globally unique, so the poll id doubles as the job id — no
    // id-generator handle is needed at this point of the transaction.
    let job = Job::new(
        poll_id,
        JobKind::PollExpired,
        serde_json::json!({ "poll_id": poll_id.value() }),
        expires_at_ms,
        now_ms,
    );
    super::jobs::insert_job(write, &job)
}

impl PollStore for RedbStore {
    fn poll_by_id(&self, id: Id) -> Result<Option<Poll>> {
        let read = self.begin_read()?;
        let polls = read.open_table(POLLS)?;
        match polls.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn poll_by_status(&self, status_id: Id) -> Result<Option<Poll>> {
        let read = self.begin_read()?;
        let poll_id = {
            let by_status = read.open_table(POLL_BY_STATUS)?;
            match by_status.get(status_id.value())? {
                Some(guard) => Id::new(guard.value()),
                None => return Ok(None),
            }
        };
        let polls = read.open_table(POLLS)?;
        match polls.get(poll_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn ballot(&self, poll_id: Id, account_id: Id) -> Result<Option<Vec<u64>>> {
        let read = self.begin_read()?;
        let votes = read.open_table(POLL_VOTES)?;
        match votes.get(schema::pair_key(poll_id.value(), account_id.value()).as_slice())? {
            Some(guard) => {
                let vote: PollVote = decode(guard.value())?;
                Ok(Some(vote.choices))
            }
            None => Ok(None),
        }
    }

    fn polls_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Poll>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let read = self.begin_read()?;
        let polls = read.open_table(POLLS)?;
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            if let Some(guard) = polls.get(id.value())? {
                out.insert(*id, decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn ballots(&self, poll_ids: &[Id], account_id: Id) -> Result<HashMap<Id, Vec<u64>>> {
        if poll_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let read = self.begin_read()?;
        let votes = read.open_table(POLL_VOTES)?;
        let mut out = HashMap::with_capacity(poll_ids.len());
        for poll_id in poll_ids {
            if out.contains_key(poll_id) {
                continue;
            }
            let key = schema::pair_key(poll_id.value(), account_id.value());
            if let Some(guard) = votes.get(key.as_slice())? {
                let vote: PollVote = decode(guard.value())?;
                out.insert(*poll_id, vote.choices);
            }
        }
        Ok(out)
    }

    fn vote_poll(
        &self,
        poll_id: Id,
        account_id: Id,
        choices: &[u64],
        now_ms: u64,
    ) -> std::result::Result<Poll, VoteRejection> {
        let write = self.begin_write().map_err(vote_err)?;

        // One transaction: validation, tallies, ballot row and the
        // vote-notification job commit (or abort) together.
        let mut poll = match read_poll_txn(&write, poll_id).map_err(vote_err)? {
            Some(poll) => poll,
            // The use case resolves the poll before voting, so a missing row
            // means it vanished concurrently: same outcome class as expired.
            None => return Err(VoteRejection::Expired),
        };
        if poll.is_expired_at(now_ms) {
            return Err(VoteRejection::Expired);
        }
        if account_id == poll.account_id {
            return Err(VoteRejection::SelfVote);
        }
        if choices.is_empty() {
            return Err(VoteRejection::InvalidChoice);
        }
        if choices.iter().any(|&c| c >= poll.options.len() as u64) {
            return Err(VoteRejection::InvalidChoice);
        }

        // Deduplicate, keeping the caller's order.
        let mut requested = Vec::with_capacity(choices.len());
        for &c in choices {
            if !requested.contains(&c) {
                requested.push(c);
            }
        }

        let vote_key = schema::pair_key(poll_id.value(), account_id.value());
        let previous: Option<PollVote> = {
            let votes = write.open_table(POLL_VOTES).map_err(vote_err)?;
            match votes.get(vote_key.as_slice()).map_err(vote_err)? {
                Some(guard) => Some(decode(guard.value()).map_err(vote_err)?),
                None => None,
            }
        };
        let ballot_created_at_ms = previous.as_ref().map_or(now_ms, |p| p.created_at_ms);

        let (final_choices, is_new_ballot) = match previous {
            None => {
                for &c in &requested {
                    poll.options[c as usize].votes_count += 1;
                }
                poll.votes_count += 1;
                poll.voters_count += 1;
                (requested, true)
            }
            Some(prev) => {
                if !poll.multiple {
                    // Mastodon's `already_voted_on_non_multiple_poll?`: any
                    // second vote on a single-choice poll is rejected — there
                    // are no ballot changes on single-choice polls.
                    return Err(VoteRejection::AlreadyVoted);
                }
                // Multiple polls reject only choices already cast
                // (`already_voted_for_same_choice_on_multiple_poll?`); new
                // choices are ADDED to the ballot.
                if requested.iter().any(|c| prev.choices.contains(c)) {
                    return Err(VoteRejection::AlreadyVoted);
                }
                let mut merged = prev.choices.clone();
                merged.extend(requested.iter().copied());
                for &c in &requested {
                    poll.options[c as usize].votes_count += 1;
                }
                poll.votes_count += requested.len() as u64;
                // `voters_count` counts ballots, not choices: unchanged.
                (merged, false)
            }
        };

        let ballot = PollVote {
            poll_id,
            account_id,
            choices: final_choices,
            created_at_ms: ballot_created_at_ms,
        };
        {
            let mut votes = write.open_table(POLL_VOTES).map_err(vote_err)?;
            votes
                .insert(vote_key.as_slice(), encode(&ballot).map_err(vote_err)?.as_slice())
                .map_err(vote_err)?;
        }
        {
            let mut polls = write.open_table(POLLS).map_err(vote_err)?;
            polls
                .insert(poll_id.value(), encode(&poll).map_err(vote_err)?.as_slice())
                .map_err(vote_err)?;
        }

        // Only a first ballot notifies the poll author; a changed ballot is
        // silent. The fan-out itself runs post-commit, as a job.
        if is_new_ballot {
            let job = Job::new(
                self.next_id(),
                JobKind::FanOutNotification,
                fan_out_payload(
                    poll.account_id,
                    NotificationType::PollVote,
                    Some(account_id),
                    Some(poll.status_id),
                    now_ms,
                ),
                now_ms,
                now_ms,
            );
            super::jobs::insert_job(&write, &job).map_err(vote_err)?;
        }

        // Federation, committed with the tally: a local vote on a remote poll
        // is delivered to its origin (`poll_vote`); a vote on a local poll
        // redistributes the new totals to followers (`question_update`).
        // Each event is a no-op for a remote actor, so it lands exactly
        // where it is due: a remote voter's ballot never leaves, and a poll
        // we do not own never gets an update from us.
        super::federation_content::federation_event_txn(
            &write,
            self.next_id(),
            account_id,
            serde_json::json!({"event": "poll_vote", "status_id": poll.status_id.value()}),
            now_ms,
        )
        .map_err(vote_err)?;
        super::federation_content::federation_event_txn(
            &write,
            self.next_id(),
            poll.account_id,
            serde_json::json!({"event": "question_update", "status_id": poll.status_id.value()}),
            now_ms,
        )
        .map_err(vote_err)?;

        write.commit().map_err(vote_err)?;
        Ok(poll)
    }

    fn mark_poll_expired(&self, poll_id: Id, now_ms: u64) -> Result<Option<Poll>> {
        let _ = now_ms; // The sticky flag records expiry; no timestamp column.
        let write = self.begin_write()?;
        let mut poll = match read_poll_txn(&write, poll_id)? {
            Some(poll) => poll,
            // Idempotent: a replayed PollExpired job finds nothing to do.
            None => return Ok(None),
        };
        poll.expired = true;
        {
            let mut polls = write.open_table(POLLS)?;
            polls.insert(poll_id.value(), encode(&poll)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(poll))
    }

    fn sync_remote_poll(
        &self,
        poll_id: Id,
        counts: &[(String, u64)],
        voters_count: u64,
        expires_at_ms: Option<u64>,
        expired: bool,
        now_ms: u64,
    ) -> Result<Option<Poll>> {
        let write = self.begin_write()?;
        let mut poll = match read_poll_txn(&write, poll_id)? {
            Some(poll) => poll,
            None => return Ok(None),
        };
        for (title, votes) in counts {
            if let Some(option) = poll.options.iter_mut().find(|o| o.title == *title) {
                option.votes_count = *votes;
            }
        }
        // The origin is authoritative for the totals; local ballots stay as
        // they are (a remote sync never invents or removes a local ballot).
        poll.voters_count = voters_count;
        poll.votes_count = poll.options.iter().map(|o| o.votes_count).sum();
        if let Some(expiry) = expires_at_ms {
            poll.expires_at_ms = Some(expiry);
        }
        if expired || poll.is_expired_at(now_ms) {
            poll.expired = true;
        }
        {
            let mut polls = write.open_table(POLLS)?;
            polls.insert(poll_id.value(), encode(&poll)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(poll))
    }
}
