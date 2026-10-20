use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::application::streaming::StreamEvent;
use crate::domain::{
    AccessToken, Account, AccountNote, AccountStats, AuthorizationCode, Block, CustomEmoji,
    CustomFilter, DomainBlock, Endorsement, FilterKeyword, FilterStatus, Follow, Id, Job, JobKind,
    List, LocalUser, LoginSession, Marker, MediaAttachment, Mute, Notification, NotificationType,
    OAuthApplication, Poll, QuotePolicy, QuoteRecord, RepliesPolicy, Result, ScheduledStatus,
    Status, StatusApplication, StatusVersion, Visibility,
};

// Re-export the two kernel ports under their canonical names.
pub use crate::domain::{Clock, IdGenerator};

/// Repository inputs and results shared by the ports below.

/// Input for creating a local account and its credential together.
pub struct NewLocalAccount {
    pub username: String,
    pub normalized_username: String,
    pub domain: String,
    pub display_name: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub actor_uri: String,
    pub now_ms: u64,
    /// When true, a `SendConfirmationEmail` job is committed in the same
    /// transaction as the account (public registration). The CLI bootstrap
    /// leaves this false.
    pub send_confirmation_email: bool,
}

/// Input for registering an OAuth application.
pub struct NewApp {
    pub name: String,
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub scopes: Vec<String>,
    pub website: Option<String>,
    pub now_ms: u64,
}

/// Input for issuing an access token (hash only; plaintext stays with caller).
pub struct NewToken {
    pub app_id: Id,
    pub user_id: Option<Id>,
    pub token_hash: String,
    pub scopes: Vec<String>,
    pub now_ms: u64,
    pub expires_at_ms: Option<u64>,
}

/// Input for creating an OAuth authorization code (hash only).
pub struct NewAuthCode {
    pub app_id: Id,
    pub user_id: Id,
    pub code_hash: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Input for creating a login session.
pub struct NewSession {
    pub user_id: Id,
    pub token_hash: String,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Mutable subset of a user's preferences (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct UserSettingsUpdate {
    pub default_privacy: Option<String>,
    pub default_sensitive: Option<bool>,
    pub default_language: Option<String>,
    pub default_quote_policy: Option<String>,
}

/// Mutable subset of an account profile (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct AccountProfileUpdate {
    pub display_name: Option<String>,
    pub note: Option<String>,
    pub locked: Option<bool>,
    pub discoverable: Option<bool>,
    pub indexable: Option<bool>,
    pub hide_collections: Option<bool>,
}

/// Accounts, local users, and the unique handle index.
pub trait AccountStore: Send + Sync {
    /// Create an account + credential + handle index entry + follow-up job(s) in
    /// one atomic transaction.
    fn create_local_account(&self, input: NewLocalAccount) -> Result<Account>;

    fn account_by_id(&self, id: Id) -> Result<Option<Account>>;

    fn account_by_handle(&self, normalized_username: &str, domain: &str) -> Result<Option<Account>>;

    fn user_by_account_id(&self, account_id: Id) -> Result<Option<LocalUser>>;

    fn local_user_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<LocalUser>>;

    fn account_count(&self) -> Result<u64>;

    /// Accounts by id in ONE transaction (page loader; missing ids are
    /// omitted). Duplicates in `ids` are read once.
    fn accounts_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Account>>;

    /// Apply a profile patch, returning the updated account.
    fn update_account_profile(
        &self,
        account_id: Id,
        update: AccountProfileUpdate,
    ) -> Result<Account>;

    /// Apply a preferences patch, returning the updated credential record.
    fn update_user_settings(&self, account_id: Id, update: UserSettingsUpdate) -> Result<LocalUser>;
}

/// OAuth applications, authorization codes and access tokens.
pub trait OAuthStore: Send + Sync {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication>;
    fn app_by_client_id(&self, client_id: &str) -> Result<Option<OAuthApplication>>;
    /// Resolve an application by its numeric id (used to attach the owning app
    /// to an access token).
    fn app_by_id(&self, id: Id) -> Result<Option<OAuthApplication>>;
    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken>;
    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>>;
    fn revoke_token(&self, plaintext: &str) -> Result<bool>;

    fn create_authorization_code(&self, input: NewAuthCode) -> Result<AuthorizationCode>;
    fn authorization_code_by_plaintext(&self, plaintext: &str) -> Result<Option<AuthorizationCode>>;
    /// Atomically mark the code used and issue its token. Returns an error if
    /// the code is missing, already used, or expired — so a replay can never
    /// mint a second token.
    fn redeem_authorization_code(
        &self,
        plaintext: &str,
        token: NewToken,
        now_ms: u64,
    ) -> Result<AccessToken>;
}

/// Server-side login sessions for the OAuth approval flow.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, input: NewSession) -> Result<LoginSession>;
    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>>;
    fn revoke_session(&self, plaintext: &str) -> Result<bool>;
}

/// Outbound mail (verification messages). An adapter/job boundary: mail is
/// never sent inside a request transaction.
pub trait MailSender: Send + Sync {
    /// Send an email-confirmation message for a newly registered account.
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
}

/// Blob/object storage for media bytes and custom emoji images.
///
/// File storage cannot participate in a `redb` transaction: bytes are staged
/// *before* the metadata row commits, and a failed commit leaves only a
/// bounded orphan for cleanup (`birchtree.md` §5).
pub trait BlobStore: Send + Sync {
    /// Stage `bytes` under `key` (creates parents; overwrites).
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;
    /// Read a blob (`None` when missing).
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// Delete a blob (idempotent).
    fn delete(&self, key: &str) -> Result<()>;
    /// Whether a blob exists.
    fn exists(&self, key: &str) -> Result<bool>;
}

/// Durable job queue.
pub trait JobQueue: Send + Sync {
    fn enqueue(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        now_ms: u64,
    ) -> Result<Job>;

    /// Enqueue a job that becomes claimable at `due_at_ms` (scheduled-status
    /// publication, delayed retries).
    fn enqueue_at(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        due_at_ms: u64,
        now_ms: u64,
    ) -> Result<Job>;
    /// Claim up to `limit` due jobs under a lease.
    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>>;
    fn complete(&self, id: Id) -> Result<()>;
    /// Record a failed attempt; schedules a retry or quarantines.
    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job>;
    fn quarantine(&self, id: Id, error: &str) -> Result<()>;
    fn get(&self, id: Id) -> Result<Option<Job>>;
}

/// Readiness probe used by the runtime.
pub trait ReadyCheck: Send + Sync {
    /// True when the store is open and can serve a read.
    fn is_ready(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Phase 3 ports: statuses/interactions, relationships, polls, quotes,
// notifications, media metadata.
//
// Invariants every mutating method upholds (see `birchtree.md` §5):
// one `redb` transaction commits the record mutation, affected indexes,
// counters, and any bounded durable jobs atomically; network-facing work
// (notification fan-out, federation) runs after commit as jobs.
// ---------------------------------------------------------------------------

/// A poll to embed in a created or edited status.
#[derive(Debug, Clone)]
pub struct NewPollSpec {
    pub multiple: bool,
    pub hide_totals: bool,
    /// Absolute expiry; local polls always have one.
    pub expires_at_ms: u64,
    /// 2–4 option titles, validated by the use case.
    pub options: Vec<String>,
}

/// A quote to attach at status creation (local quoted statuses in Phase 3).
#[derive(Debug, Clone)]
pub struct NewQuoteSpec {
    pub quoted_status_id: Id,
    /// Author of the quoted status (for the silent mention + counter update).
    pub quoted_account_id: Id,
    /// The quoting status's `quote_approval_policy`.
    pub approval_policy: QuotePolicy,
}

/// Input for creating a status in one atomic transaction.
/// The URI and URL of a remote status, as its server published them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatusIdentity {
    pub uri: String,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewStatus {
    /// Set for a status received from a remote server: its own URI and URL are
    /// stored as given, and no local URI is minted. `None` for a local status.
    pub remote: Option<RemoteStatusIdentity>,
    pub account_id: Id,
    /// Canonical ActivityPub object URI base; the store appends `/{id}`
    /// (computed by the use case from the public URL + username).
    pub uri_base: String,
    /// Local web URL base; the store appends `/{id}`.
    pub url_base: String,
    pub source_text: String,
    pub content_html: String,
    pub spoiler_text: Option<String>,
    pub sensitive: bool,
    pub visibility: Visibility,
    pub language: Option<String>,
    pub in_reply_to_id: Option<Id>,
    pub in_reply_to_account_id: Option<Id>,
    /// Attached media ids, in the author's order.
    pub media_ids: Vec<Id>,
    /// Resolved mention account ids, in mention order (serialized).
    pub mentions: Vec<Id>,
    /// Resolved silent mentions (quote access grants): visibility only.
    pub silent_mentions: Vec<Id>,
    /// Lowercased hashtags without the `#` prefix.
    pub tags: Vec<String>,
    pub poll: Option<NewPollSpec>,
    pub quote: Option<NewQuoteSpec>,
    pub application: Option<StatusApplication>,
    /// Mastodon `Idempotency-Key` header value, when present.
    pub idempotency_key: Option<String>,
}

/// Result of a status creation; a replayed idempotent retry returns the
/// originally created status with `replayed = true`.
#[derive(Debug, Clone)]
pub struct CreatedStatus {
    pub status: Status,
    pub replayed: bool,
}

/// A deletion's pre-deletion snapshots (see [`StatusStore::delete_status`]).
#[derive(Debug, Clone)]
pub struct DeletedStatus {
    pub status: Status,
    /// Reblog wrappers tombstoned by the same transaction.
    pub reblogs: Vec<Status>,
}

/// Field-level edit patch (`PUT /api/v1/statuses/:id`). `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct StatusUpdate {
    pub source_text: Option<String>,
    pub content_html: Option<String>,
    /// `Some(None)` clears a spoiler; `None` leaves it unchanged.
    pub spoiler_text: Option<Option<String>>,
    pub sensitive: Option<bool>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<Id>>,
    /// Replace poll options/multiple (resets all votes, per Mastodon).
    pub poll: Option<NewPollSpec>,
    /// Extend poll expiry without resetting votes.
    pub poll_expires_in_ms: Option<u64>,
    pub quote_approval_policy: Option<QuotePolicy>,
}

/// Per-viewer status flags for DTO packing.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusViewFlags {
    pub favourited: bool,
    pub bookmarked: bool,
    /// The viewer has reblogged the (proper) status.
    pub reblogged: bool,
    /// The viewer has pinned the status.
    pub pinned: bool,
}

/// Cursor-bounded query for an account's statuses.
#[derive(Debug, Clone, Copy)]
pub struct AccountStatusesQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// Return the account's pinned statuses in pin order instead.
    pub pinned_only: bool,
    pub exclude_replies: bool,
    pub exclude_reblogs: bool,
}

/// Statuses, replies, reblogs, edits, pins and status interactions.
pub trait StatusStore: Send + Sync {
    /// Create a status with its poll/quote, indexes, counters, idempotency
    /// record and follow-up jobs in ONE transaction.
    fn create_status(&self, input: NewStatus, now_ms: u64) -> Result<CreatedStatus>;

    fn status_by_id(&self, id: Id) -> Result<Option<Status>>;

    /// Non-deleted statuses among `ids`, ordered by id descending.
    fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>>;

    /// Apply an edit: snapshot the pre-edit version (once), patch fields, bump
    /// `edited_at_ms`, keep counters — ONE transaction. Returns `Ok(None)`
    /// when the patch changed nothing (no snapshot, no `edited_at` bump).
    fn update_status(
        &self,
        id: Id,
        account_id: Id,
        update: StatusUpdate,
        now_ms: u64,
    ) -> Result<Option<Status>>;

    /// Tombstone a status in ONE transaction (tombstone + dependent reblogs +
    /// pin removal + parent/author counter fixes + follow-up job). Returns the
    /// pre-deletion snapshots: the status and every reblog wrapper the cascade
    /// tombstoned (streaming needs them to send `delete` events). `NotFound`
    /// when missing, deleted, or not owned.
    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<DeletedStatus>;

    /// The live reblog wrapper `account_id` created of `status_id`, if any.
    fn own_reblog(&self, account_id: Id, status_id: Id) -> Result<Option<Status>>;

    /// Direct reply children of `parent_id`, newest first, cursor-bounded.
    fn status_children(
        &self,
        parent_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Status>>;

    /// Edit-history snapshots, oldest first (empty when never edited).
    fn status_versions(&self, status_id: Id) -> Result<Vec<StatusVersion>>;

    /// Statuses authored by `account_id` (non-deleted), id-descending with
    /// cursors; `pinned_only` returns the account's pins in pin order.
    fn statuses_by_account(
        &self,
        account_id: Id,
        query: AccountStatusesQuery,
    ) -> Result<Vec<Status>>;

    /// Viewer flags for a batch of statuses (no N+1).
    fn viewer_status_flags(
        &self,
        viewer_id: Id,
        status_ids: &[Id],
    ) -> Result<HashMap<Id, StatusViewFlags>>;

    // -- Interactions (each ONE transaction: row + counters + follow-up job) --

    /// Add a favourite (idempotent: already-favourited returns the status
    /// unchanged). NotFound when the status is missing or not visible.
    fn favourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Remove a favourite (no-op when absent), returning the status.
    fn unfavourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    fn bookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    fn unbookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Create (or return the existing) reblog by `account_id` of `status_id`.
    fn reblog_status(
        &self,
        account_id: Id,
        status_id: Id,
        visibility: Visibility,
        now_ms: u64,
    ) -> Result<Status>;
    /// Delete the viewer's reblog of `status_id`, returning the original
    /// (counters decremented; no-op when the viewer never reblogged).
    fn unreblog_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Pin own status (limit enforced by the use case), returning the status.
    fn pin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Unpin own status (no-op when not pinned), returning the status.
    fn unpin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Update only the quote approval policy — no edit snapshot and no
    /// `edited_at` bump (mirrors Mastodon's interaction-policy endpoint).
    /// NotFound when missing/deleted; the caller enforces ownership.
    fn set_quote_policy(&self, status_id: Id, policy: QuotePolicy) -> Result<Status>;

    /// Accounts that favourited `status_id`, newest first, as
    /// `(account_id, favourite_id)` pairs; the interaction id is the cursor.
    fn favourited_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;
    /// Accounts that reblogged `status_id`, newest first, as
    /// `(account_id, reblog_status_id)` pairs; the reblog id is the cursor.
    fn reblogged_by(&self, status_id: Id, max_id: Option<Id>, limit: usize)
    -> Result<Vec<(Id, Id)>>;
}

/// Follows, follow requests, blocks, domain blocks, mutes, notes, endorsements
/// and the relationship aggregate.
#[derive(Debug, Clone)]
pub struct FollowOptions {
    pub show_reblogs: bool,
    pub notify: bool,
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
    fn remove_follower(&self, account_id: Id, follower_id: Id, now_ms: u64) -> Result<Option<Follow>>;

    /// Pending inbound follow requests, newest first (cursor = follower id).
    fn follow_requests(
        &self,
        target_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Follow>>;

    /// Convert a pending request to an active follow (atomic + notification
    /// job); NotFound when no request exists.
    fn authorize_follow_request(&self, target_id: Id, follower_id: Id, now_ms: u64)
    -> Result<Follow>;

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
}

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

/// Quotes (local quote state machine; federated quote requests are Phase 6).
pub trait QuoteStore: Send + Sync {
    /// The quote authored by a quoting status, if any.
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>>;

    /// Quote records keyed by quoting status id in ONE transaction (page
    /// loader; statuses without a quote are omitted).
    fn quotes_by_quoting_ids(&self, ids: &[Id]) -> Result<HashMap<Id, QuoteRecord>>;

    /// Accepted quotes of `quoted_status_id` with their quoting statuses,
    /// newest first (cursor = quoting status id).
    fn quotes_of_status(
        &self,
        quoted_status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(QuoteRecord, Status)>>;

    /// Revoke an accepted quote (quoted author only): state -> `revoked`,
    /// quoted counter decrement — ONE transaction. NotFound when no such
    /// quote; Forbidden handled by the use case before the call.
    fn revoke_quote(
        &self,
        quoted_status_id: Id,
        quoting_status_id: Id,
        now_ms: u64,
    ) -> Result<QuoteRecord>;
}

/// Notification records (fan-out happens after commit, via jobs).
#[derive(Debug, Clone)]
pub struct NewNotification {
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub created_at_ms: u64,
}

pub trait NotificationStore: Send + Sync {
    /// Insert a notification unless its dedup key `(recipient, type, actor,
    /// status)` already exists; returns `None` on a dedup hit. Used by the
    /// `FanOutNotification` job handler, so job retries are harmless.
    fn create_notification(&self, input: NewNotification) -> Result<Option<Notification>>;

    /// One page for the recipient, newest first; cursors exclusive
    /// (`id < max_id`, `id > since_id`/`id > min_id`), bounded scan.
    fn notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>>;

    fn notification_by_id(&self, id: Id) -> Result<Option<Notification>>;

    /// Delete one of the recipient's notifications; `false` when missing or
    /// belonging to someone else.
    fn delete_notification(&self, recipient_id: Id, id: Id) -> Result<bool>;

    /// Delete every notification of the recipient.
    fn clear_notifications(&self, recipient_id: Id) -> Result<()>;

    /// Count notifications with `id > floor`, capped by `limit` (the
    /// marker-based `unread_count`).
    fn count_notifications_after(
        &self,
        recipient_id: Id,
        floor: Option<Id>,
        limit: u64,
    ) -> Result<u64>;
}

/// Media attachment metadata (bytes/lifecycle are Phase 4).
#[derive(Debug, Clone)]
pub struct NewMedia {
    pub account_id: Id,
    /// One of `unknown`/`image`/`gifv`/`video`/`audio`.
    pub r#type: String,
    pub description: String,
}

/// An uploaded attachment whose bytes are already staged in the blob store.
///
/// The store commits metadata + the `ProcessMedia` job in ONE transaction; the
/// job finishes processing after commit (`birchtree.md` §5).
#[derive(Debug, Clone)]
pub struct NewMediaUpload {
    pub account_id: Id,
    /// Wire `type`: `image`/`video`/`audio`/`gifv`.
    pub r#type: String,
    pub mime_type: String,
    pub description: String,
    pub file_size: u64,
    /// Blob key of the staged original.
    pub file_key: String,
    /// Cover image blob key (video/audio).
    pub thumbnail_key: Option<String>,
    /// Focus point (`focus` param).
    pub focus: Option<crate::domain::FocusPoint>,
    /// Delayed processing (v2 create for larger formats): the attachment stays
    /// `Pending` until the `ProcessMedia` job completes.
    pub delay: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
}

/// Processing result committed by the `ProcessMedia` job (idempotent).
#[derive(Debug, Clone)]
pub struct MediaProcessingResult {
    /// `Ready` (or `Failed` with `error`).
    pub state: crate::domain::MediaState,
    pub r#type: String,
    pub blurhash: Option<String>,
    /// Blob key of the generated `small` preview.
    pub preview_key: Option<String>,
    pub preview_width: Option<u32>,
    pub preview_height: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
    pub error: Option<String>,
}

/// Owner-scoped media patch (`PUT /api/v1/media/:id`).
#[derive(Debug, Clone, Default)]
pub struct MediaPatch {
    pub description: Option<String>,
    /// `Some(None)` clears the focus point; `None` leaves it unchanged.
    pub focus: Option<Option<crate::domain::FocusPoint>>,
    /// Replace the cover image blob key (`None` leaves it unchanged).
    pub thumbnail_key: Option<Option<String>>,
}

pub trait MediaStore: Send + Sync {
    fn create_media(&self, input: NewMedia) -> Result<MediaAttachment>;
    /// The subset of `ids` owned by `owner_id` and unattached (or already
    /// attached to `status_id`), returned in the requested order.
    fn attachable_media(&self, owner_id: Id, ids: &[Id], status_id: Option<Id>)
    -> Result<Vec<MediaAttachment>>;
    fn media_by_id(&self, id: Id) -> Result<Option<MediaAttachment>>;

    /// Attachments by id in ONE transaction (page loader; missing ids omitted,
    /// duplicates read once).
    fn media_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, MediaAttachment>>;

    /// Insert an uploaded attachment's metadata + `ProcessMedia` job in ONE
    /// transaction.
    fn create_media_upload(&self, input: NewMediaUpload, now_ms: u64) -> Result<MediaAttachment>;

    /// The owner's media row (Mastodon `set_media_attachment`: only the
    /// owner's own, not-yet-attached rows are addressable; others are `None`
    /// → 404).
    fn owner_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Apply an owner-scoped patch (description/focus/thumbnail).
    fn update_media(
        &self,
        account_id: Id,
        id: Id,
        patch: MediaPatch,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;

    /// Delete an owner's unattached attachment (bytes are removed by the
    /// caller after commit). `None` when missing/foreign; the use case
    /// rejects attached rows with the Mastodon in-usage error.
    fn delete_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Commit a processing result (idempotent; `None` when the row is gone).
    fn mark_media_processed(
        &self,
        id: Id,
        result: MediaProcessingResult,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;
}

// ---------------------------------------------------------------------------
// Phase 4 ports: lists, markers, filters, scheduled statuses, custom emoji,
// timeline scans. Same invariants as the Phase 3 ports: one `redb`
// transaction per method, mutating methods commit record + indexes +
// counters + bounded durable jobs atomically.
// ---------------------------------------------------------------------------

pub trait ListStore: Send + Sync {
    /// Create a list for `account_id` (the use case enforces the per-account
    /// limit and title rules).
    fn create_list(
        &self,
        account_id: Id,
        title: &str,
        replies_policy: RepliesPolicy,
        exclusive: bool,
        now_ms: u64,
    ) -> Result<List>;
    fn list_by_id(&self, id: Id) -> Result<Option<List>>;
    /// The owner's lists (index returns all; no pagination in v4.7.3).
    fn lists_by_account(&self, account_id: Id) -> Result<Vec<List>>;
    /// Patch a list the viewer owns; `None` when missing/foreign (404).
    fn update_list(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        replies_policy: Option<RepliesPolicy>,
        exclusive: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<List>>;
    /// Delete a list the viewer owns (and its memberships); `None` when
    /// missing/foreign.
    fn delete_list(&self, account_id: Id, id: Id) -> Result<bool>;

    /// Add members (must follow, or have a pending request — enforced by the
    /// use case). Idempotent: existing members are skipped; the returned ids
    /// are the newly added ones.
    fn add_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Remove members (idempotent); returns the removed ids.
    fn remove_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Members of a list, newest first (cursor = member account id).
    fn list_members(&self, list_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    /// All members of a list (bounded), for the list timeline merge.
    fn all_list_members(&self, list_id: Id, cap: usize) -> Result<Vec<Id>>;
    /// The `candidates` that belong to one of `owner`'s lists with
    /// `exclusive = true` (upstream's `exclusive_list_users`, which hides them
    /// from home). One read transaction; no scan cap, because the cost is the
    /// owner's exclusive lists times the candidates.
    fn exclusive_list_members(&self, owner: Id, candidates: &[Id]) -> Result<HashSet<Id>>;
}

/// Why a marker update was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerUpdateError {
    /// Unknown or invalid timeline name.
    InvalidTimeline,
    /// `lock_version` did not match (Mastodon answers 409).
    Stale,
}

pub trait MarkerStore: Send + Sync {
    /// The account's markers for `timelines` (missing ones are omitted).
    fn markers(&self, account_id: Id, timelines: &[&str]) -> Result<Vec<Marker>>;
    /// Upsert a marker with optimistic locking: when `lock_version` is given
    /// it must match the stored version, else `Stale`.
    fn set_marker(
        &self,
        account_id: Id,
        timeline: &str,
        last_read_id: Id,
        lock_version: Option<u64>,
        now_ms: u64,
    ) -> std::result::Result<Marker, MarkerUpdateError>;
}

/// A filter to create (v2 record; v1 create wraps exactly one keyword).
#[derive(Debug, Clone)]
pub struct NewFilter {
    pub account_id: Id,
    pub title: String,
    pub context: Vec<String>,
    pub expires_at_ms: Option<u64>,
    pub action: crate::domain::FilterAction,
    /// Initial keywords (`v2 keywords_attributes` or the v1 `phrase`).
    pub keywords: Vec<(String, bool)>,
}

/// Field-level filter patch; `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct FilterPatch {
    pub title: Option<String>,
    pub context: Option<Vec<String>>,
    /// `Some(None)` clears the expiry.
    pub expires_at_ms: Option<Option<u64>>,
    pub action: Option<crate::domain::FilterAction>,
    /// Replace keywords wholesale (`v2 keywords_attributes`): `(id, keyword,
    /// whole_word, destroy)`.
    pub keywords: Option<Vec<(Option<Id>, String, bool, bool)>>,
}

/// A filter with its rules, as every filter read returns it.
#[derive(Debug, Clone)]
pub struct FilterBundle {
    pub filter: CustomFilter,
    pub keywords: Vec<FilterKeyword>,
    pub statuses: Vec<FilterStatus>,
}

pub trait FilterStore: Send + Sync {
    fn create_filter(&self, input: NewFilter, now_ms: u64) -> Result<FilterBundle>;
    fn filter_by_id(&self, id: Id) -> Result<Option<FilterBundle>>;
    /// The account's filters (optionally narrowed to a context), bounded.
    fn filters_by_account(&self, account_id: Id, context: Option<&str>)
    -> Result<Vec<FilterBundle>>;
    /// Patch an owned filter; `None` when missing/foreign.
    fn update_filter(
        &self,
        account_id: Id,
        id: Id,
        patch: FilterPatch,
        now_ms: u64,
    ) -> Result<Option<FilterBundle>>;
    fn delete_filter(&self, account_id: Id, id: Id) -> Result<bool>;

    fn keyword_by_id(&self, id: Id) -> Result<Option<FilterKeyword>>;
    /// Add a keyword to an owned filter (id = the v1 "filter" id).
    fn create_keyword(
        &self,
        account_id: Id,
        filter_id: Id,
        keyword: &str,
        whole_word: bool,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn update_keyword(
        &self,
        account_id: Id,
        id: Id,
        keyword: Option<&str>,
        whole_word: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn delete_keyword(&self, account_id: Id, id: Id) -> Result<Option<FilterKeyword>>;

    fn filter_statuses(&self, filter_id: Id) -> Result<Vec<FilterStatus>>;
    fn create_filter_status(
        &self,
        account_id: Id,
        filter_id: Id,
        status_id: Id,
        now_ms: u64,
    ) -> Result<Option<FilterStatus>>;
    fn delete_filter_status(&self, account_id: Id, id: Id) -> Result<Option<FilterStatus>>;
}

/// A scheduled status to create in ONE transaction (record + indexes + the
/// `PublishScheduledStatus` job).
#[derive(Debug, Clone)]
pub struct NewScheduledStatus {
    pub account_id: Id,
    pub scheduled_at_ms: u64,
    pub media_ids: Vec<Id>,
    pub params: serde_json::Value,
    /// Public URL base, carried into the job payload so the publish handler
    /// can replay the params through the normal creation use case.
    pub base_url: String,
}

pub trait ScheduledStore: Send + Sync {
    fn create_scheduled(&self, input: NewScheduledStatus, now_ms: u64) -> Result<ScheduledStatus>;
    fn scheduled_by_id(&self, id: Id) -> Result<Option<ScheduledStatus>>;
    /// The account's scheduled statuses, id-descending with cursors.
    fn scheduled_by_account(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<ScheduledStatus>>;
    /// Reschedule (only `scheduled_at` is editable); `None` when
    /// missing/foreign.
    fn reschedule(
        &self,
        account_id: Id,
        id: Id,
        scheduled_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ScheduledStatus>>;
    /// Delete a scheduled status (publish job payload becomes a no-op).
    fn delete_scheduled(&self, account_id: Id, id: Id) -> Result<bool>;
    /// Rows due for publication at `now_ms` (bounded).
    fn due_scheduled(&self, now_ms: u64, cap: usize) -> Result<Vec<ScheduledStatus>>;
    /// `(total, today)` counts against Mastodon's 300/25 limits.
    fn scheduled_counts(&self, account_id: Id, day_start_ms: u64, day_end_ms: u64)
    -> Result<(usize, usize)>;
}

pub trait EmojiStore: Send + Sync {
    /// Create or replace an emoji by shortcode (CLI-managed).
    fn upsert_emoji(&self, emoji: CustomEmoji) -> Result<CustomEmoji>;
    fn emoji_by_shortcode(&self, shortcode: &str) -> Result<Option<CustomEmoji>>;
    fn delete_emoji(&self, shortcode: &str) -> Result<bool>;
    /// Listed emoji (index endpoint; local + enabled + picker-visible),
    /// shortcode order, bounded.
    fn listed_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>>;
    /// Emoji whose shortcode is in `shortcodes` (status/account payloads).
    fn emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<CustomEmoji>>;
}

/// Bounded local search scans backing `GET /api/v2/search`.
pub trait SearchStore: Send + Sync {
    /// Accounts whose lowercase handle or display name contains `needle`
    /// (bounded).
    fn search_accounts(&self, needle: &str, cap: usize) -> Result<Vec<Account>>;

    /// Distinct hashtags whose normalized name starts with `prefix`
    /// (bounded; derived from the hashtag index).
    fn search_tags(&self, prefix: &str, cap: usize) -> Result<Vec<String>>;

    /// Recent statuses (optionally one author's), id-descending with
    /// cursors, capped (bounded recent scan for text search).
    fn recent_statuses(
        &self,
        account_id: Option<Id>,
        max_id: Option<Id>,
        min_id: Option<Id>,
        cap: usize,
    ) -> Result<Vec<Status>>;
}

/// Filters a candidate status through the *viewer's* perspective while a
/// bounded timeline scan fills a page. Implemented in
/// `application::timelines` (the policy stays in the application layer); the
/// store only applies it inside its scan budget.
pub trait TimelineFilter: Send + Sync {
    fn include(&self, status: &Status) -> bool;
}

/// Cursor-bounded timeline query (limit is already endpoint-bounded).
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// `only_media` (public/tag timelines).
    pub only_media: bool,
    /// `local=true` (public/tag timelines).
    pub local_only: bool,
    /// `remote=true` (public/tag timelines).
    pub remote_only: bool,
}

/// One page of a timeline plus the number of index entries the bounded scan
/// walked (executed evidence for the no-unbounded-scan tests).
#[derive(Debug, Clone, Default)]
pub struct TimelinePage {
    pub statuses: Vec<Status>,
    pub scanned: usize,
}

pub trait TimelineStore: Send + Sync {
    /// Public timeline: `public`-visibility statuses, newest first. Replies
    /// (other-than-self) and reblogs are excluded unless the flags say
    /// otherwise (tag timelines include both).
    fn public_timeline(
        &self,
        query: TimelineQuery,
        include_replies: bool,
        include_reblogs: bool,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Hashtag timeline for an already-normalized tag.
    fn hashtag_timeline(
        &self,
        tag: &str,
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Fan-out-on-read merge over `authors` (own + followed accounts for
    /// home; list members for a list), newest first, with per-author and
    /// total scan budgets.
    fn authors_timeline(
        &self,
        authors: &[Id],
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;
}

/// Bundled persistence surface assembled into a `Context`.
///
/// Implemented once by the `redb` adapter; application code depends on the
/// individual narrow traits above, never on `redb` types.
pub trait Store:
    AccountStore
    + OAuthStore
    + SessionStore
    + JobQueue
    + ReadyCheck
    + StatusStore
    + RelationshipStore
    + PollStore
    + QuoteStore
    + NotificationStore
    + MediaStore
    + ListStore
    + MarkerStore
    + FilterStore
    + ScheduledStore
    + EmojiStore
    + TimelineStore
    + SearchStore
    + crate::application::federation_ports::FederationStore
    + crate::application::federation_ports::FederationContentStore
{
}
impl<T> Store for T where
    T: AccountStore
        + OAuthStore
        + SessionStore
        + JobQueue
        + ReadyCheck
        + StatusStore
        + RelationshipStore
        + PollStore
        + QuoteStore
        + NotificationStore
        + MediaStore
        + ListStore
        + MarkerStore
        + FilterStore
        + ScheduledStore
        + EmojiStore
        + TimelineStore
        + SearchStore
        + crate::application::federation_ports::FederationStore
    + crate::application::federation_ports::FederationContentStore
{
}

/// Publishes committed changes to open streams (Phase 5).
///
/// Non-blocking. The hub's delivery runs on its own thread, so a slow client or
/// a slow policy read never stalls the request that committed the change. If
/// the hub's event queue overflows, the hub closes every open stream instead of
/// dropping the event silently: clients reconnect and reread state.
pub trait StreamingSink: Send + Sync {
    fn publish(&self, event: StreamEvent);
}

/// A sink that discards events. Used where no streaming hub is assembled.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStreaming;

impl StreamingSink for NoStreaming {
    fn publish(&self, _event: StreamEvent) {}
}

/// Snapshot of kernel dependencies assembled in `application::context`.
#[derive(Clone)]
pub struct ContextDeps {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

/// Dependency-assembly surface for adapters.
pub trait Ports: Send + Sync {
    fn clock(&self) -> Arc<dyn Clock>;
    fn ids(&self) -> Arc<dyn IdGenerator>;
}

impl Ports for ContextDeps {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.ids.clone()
    }
}
