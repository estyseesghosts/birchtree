use std::collections::HashMap;

use crate::domain::Id;
use crate::domain::Poll;
use crate::domain::Result;

/// Why a poll vote was rejected (mapped to Mastodon 422 messages by the use
/// case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteRejection {
    Expired,
    InvalidChoice,
    SelfVote,
    AlreadyVoted,
}

/// Polls and ballots.
pub trait PollStore: Send + Sync {
    fn poll_by_id(&self, id: Id) -> Result<Option<Poll>>;
    fn poll_by_status(&self, status_id: Id) -> Result<Option<Poll>>;

    /// The viewer's ballot on a poll, if any (for `voted`/`own_votes`).
    fn ballot(&self, poll_id: Id, account_id: Id) -> Result<Option<Vec<u64>>>;

    /// Polls by id in ONE transaction (page loader; missing ids omitted).
    fn polls_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Poll>>;

    /// The viewer's ballots for several polls in ONE transaction (page
    /// loader); polls without a ballot are omitted.
    fn ballots(&self, poll_ids: &[Id], account_id: Id) -> Result<HashMap<Id, Vec<u64>>>;

    /// Cast a ballot in ONE transaction: tallies, ballot row and the
    /// vote-notification job commit together. Mastodon semantics: any second
    /// vote on a single-choice poll is rejected; on multiple-choice polls only
    /// already-cast choices are rejected (new choices are added). Validation
    /// happens inside the transaction (expiry/choice/self/dup).
    fn vote_poll(
        &self,
        poll_id: Id,
        account_id: Id,
        choices: &[u64],
        now_ms: u64,
    ) -> std::result::Result<Poll, VoteRejection>;

    /// Sticky-mark a poll expired (PollExpired job handler; idempotent).
    fn mark_poll_expired(&self, poll_id: Id, now_ms: u64) -> Result<Option<Poll>>;

    /// Overwrite a poll's tallies from its origin in ONE transaction: option
    /// counts matched by title (unknown names ignored), the voters count, the
    /// expiry when given, and the sticky expired flag. `None` when the poll
    /// row is gone. Local ballots are never rewritten by this path.
    fn sync_remote_poll(
        &self,
        poll_id: Id,
        counts: &[(String, u64)],
        voters_count: u64,
        expires_at_ms: Option<u64>,
        expired: bool,
        now_ms: u64,
    ) -> Result<Option<Poll>>;
}
