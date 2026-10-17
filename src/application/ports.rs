use std::collections::HashMap;
use std::sync::Arc;

use crate::domain::{
    AccessToken, Account, AccountNote, AccountStats, AuthorizationCode, Block, DomainBlock,
    Endorsement, Follow, Id, Job, JobKind, LocalUser, LoginSession, MediaAttachment, Mute,
    Notification, NotificationType, OAuthApplication, Poll, QuotePolicy, QuoteRecord, Result,
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

/// Durable job queue.
pub trait JobQueue: Send + Sync {
    fn enqueue(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
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
#[derive(Debug, Clone, Default)]
pub struct NewStatus {
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
    /// pre-deletion status. `NotFound` when missing, deleted, or not owned.
    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<Status>;

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
    /// Blocked domains, newest first, cursor-bounded (cursor = domain).
    fn domain_blocks(
        &self,
        account_id: Id,
        max_domain: Option<&str>,
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
}

/// Quotes (local quote state machine; federated quote requests are Phase 6).
pub trait QuoteStore: Send + Sync {
    /// The quote authored by a quoting status, if any.
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>>;

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
}

/// Media attachment metadata (bytes/lifecycle are Phase 4).
#[derive(Debug, Clone)]
pub struct NewMedia {
    pub account_id: Id,
    /// One of `unknown`/`image`/`gifv`/`video`/`audio`.
    pub r#type: String,
    pub description: String,
}

pub trait MediaStore: Send + Sync {
    fn create_media(&self, input: NewMedia) -> Result<MediaAttachment>;
    /// The subset of `ids` owned by `owner_id` and unattached (or already
    /// attached to `status_id`), returned in the requested order.
    fn attachable_media(&self, owner_id: Id, ids: &[Id], status_id: Option<Id>)
    -> Result<Vec<MediaAttachment>>;
    fn media_by_id(&self, id: Id) -> Result<Option<MediaAttachment>>;
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
{
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
