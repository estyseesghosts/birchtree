use std::collections::HashMap;

use crate::domain::{
    AccountNote, AccountStats, Block, DomainBlock, Endorsement, Follow, Id, Mute, Result,
};

/// Follows, follow requests, blocks, domain blocks, mutes, notes, endorsements
/// and the relationship aggregate.
#[derive(Debug, Clone)]
pub struct FollowOptions {
    pub show_reblogs: bool,
    pub notify: bool,
    /// D23 per-follow language filter (normalized BCP-47 codes).
    pub languages: Option<Vec<String>>,
}

/// Outcome of a follow attempt (target locked => request).
#[derive(Debug, Clone)]
pub enum FollowOutcome {
    Following(Follow),
    Requested(Follow),
}

/// Everything `REST::RelationshipSerializer` needs for one (viewer, target) pair.
#[derive(Debug, Clone)]
pub struct Relationship {
    pub target_id: Id,
    pub following: bool,
    pub show_reblogs: bool,
    pub notifying: bool,
    /// D23 per-follow language filter (`None` = no preference).
    pub languages: Option<Vec<String>>,
    pub followed_by: bool,
    pub blocking: bool,
    pub blocked_by: bool,
    pub muting: bool,
    pub muting_notifications: bool,
    pub muting_expires_at_ms: Option<u64>,
    pub requested: bool,
    pub requested_by: bool,
    pub domain_blocking: bool,
    pub endorsed: bool,
    /// Private note the viewer keeps on the target ("" when none).
    pub note: String,
}

/// Pairwise read-authorization facts used by the visibility policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairState {
    /// `a` blocks `b`.
    pub blocking: bool,
    /// `b` blocks `a`.
    pub blocked_by: bool,
    /// `a` follows `b` (active).
    pub following: bool,
    /// `b` follows `a` (active).
    pub followed_by: bool,
    /// `a` mutes `b` (unexpired).
    pub muting: bool,
}

pub trait RelationshipStore: Send + Sync {
    /// Create (or update) a follow/follow-request row atomically with both
    /// sides' counters and the follow(-request) notification job.
    fn follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        opts: FollowOptions,
        request: bool,
        now_ms: u64,
    ) -> Result<FollowOutcome>;

    /// Remove an outgoing follow or cancel a pending request.
    fn unfollow(&self, follower_id: Id, followed_id: Id, now_ms: u64) -> Result<Option<Follow>>;

    /// Remove an inbound follow (the caller's own follower).
    fn remove_follower(
        &self,
        account_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Option<Follow>>;

    /// Pending inbound follow requests, newest first (cursor = follower id;
    /// upstream paginates `FollowRequest` row ids, which are client-opaque
    /// through `Link` — registered as a divergence, not re-keyed).
    fn follow_requests(
        &self,
        target_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Follow>>;

    /// Convert a pending request to an active follow (atomic + notification
    /// job); NotFound when no request exists.
    fn authorize_follow_request(
        &self,
        target_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Follow>;

    /// Drop a pending request (atomic).
    fn reject_follow_request(
        &self,
        target_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Option<Follow>>;

    /// Block: mutual unfollow, reject pending inbound request, drop
    /// endorsements between the pair, then insert the block — ONE transaction.
    /// Returns `None` when the block already existed.
    fn block(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>>;
    fn unblock(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>>;

    /// Mute with a duration (0 = permanent) and notification-hiding flag.
    fn mute(
        &self,
        account_id: Id,
        target_id: Id,
        duration_seconds: u64,
        hide_notifications: bool,
        now_ms: u64,
    ) -> Result<Mute>;
    fn unmute(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Mute>>;

    /// Set (or with `comment` empty, delete) the viewer's private note.
    fn set_account_note(
        &self,
        viewer_id: Id,
        target_id: Id,
        comment: &str,
        now_ms: u64,
    ) -> Result<Option<AccountNote>>;

    fn domain_block(&self, account_id: Id, domain: &str, now_ms: u64) -> Result<DomainBlock>;
    fn unblock_domain(&self, account_id: Id, domain: &str) -> Result<Option<DomainBlock>>;
    /// Blocked domains, newest first. `max_id` and `since_id` are exclusive
    /// numeric row-id cursors (Mastodon's `paginate_by_max_id`).
    fn domain_blocks(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<DomainBlock>>;

    /// Endorse (pin) an account the caller follows; NotFound otherwise.
    fn endorse(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Endorsement>;
    fn unendorse(&self, account_id: Id, target_id: Id) -> Result<Option<Endorsement>>;
    /// Endorsed account ids, newest first (cursor = endorsed account id).
    fn endorsements(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;

    /// Relationship aggregates for `(viewer, (target_id, target_domain))` pairs.
    fn relationships(
        &self,
        viewer_id: Id,
        targets: &[(Id, String)],
        now_ms: u64,
    ) -> Result<Vec<Relationship>>;

    /// Pairwise authorization facts (one lookup per pair member).
    fn pair_state(&self, a: Id, b: Id, now_ms: u64) -> Result<PairState>;

    /// The follow row of `follower_id` -> `followed_id`, if any.
    fn follow_of(&self, follower_id: Id, followed_id: Id) -> Result<Option<Follow>>;

    /// Followers of / following by `account_id`, newest first (cursor = account id).
    fn followers(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    fn following(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;

    /// Account counters (followers/following/statuses/last status/requests).
    fn account_stats(&self, account_id: Id) -> Result<AccountStats>;

    /// Account counters for many accounts in ONE transaction (page loader).
    fn account_stats_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, AccountStats>>;

    /// Batched pairwise authorization facts in ONE transaction.
    ///
    /// For each `other` the entry is `(incoming, outgoing)` with
    /// `incoming = pair_state(other, viewer)` (does `other` block/follow/mute
    /// the viewer?) and `outgoing = pair_state(viewer, other)` (does the
    /// viewer block/follow/mute `other`?). An entry exists for every `other`
    /// (no relationship rows yield the default pair state), so callers can
    /// index without existence checks.
    fn batch_pair_states(
        &self,
        viewer: Id,
        others: &[Id],
        now_ms: u64,
    ) -> Result<HashMap<Id, (PairState, PairState)>>;

    /// Blocked account ids, newest first (cursor = blocked account id).
    fn blocks_of(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    /// Mute rows, newest first (cursor = muted account id).
    fn mutes_of(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Mute>>;
}
