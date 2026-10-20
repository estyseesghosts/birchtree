//! Federation ports: the federation store and the command types it commits.
//!
//! Application code states what must happen; the adapters decide how. The
//! inbound and outbound *commands* below each commit as one `redb`
//! transaction (state, indexes, the activity dedup record and the delivery
//! job). No port method performs network I/O inside its transaction.
//!
//! Fetching and sending ActivityPub documents is the `activitypub_federation`
//! crate's job, driven by the adapters in `activitypub/` and `federation/`.

use crate::domain::federation::RefreshRecord;
use crate::domain::{Account, ActorKeys, DeliveryDestState, Follow, Id, MediaAttachment, QuoteRecord, QuoteState, Report, Result, Status};

/// The payload of one outbound delivery: which local actor signs it, where it
/// goes, and the activity that is sent. The activity is fixed at commit time,
/// so a retried job sends the same content.
#[derive(Debug, Clone)]
pub struct DeliverySpec {
    pub sender_id: Id,
    pub inbox_uri: String,
    /// The activity's canonical ID, for logs and the dedup record.
    pub activity_uri: String,
    pub body: serde_json::Value,
}

/// A remote account as the inbound pipeline read it, ready to upsert.
#[derive(Debug, Clone)]
pub struct RemoteAccountUpsert {
    pub actor_uri: String,
    pub username: String,
    /// `host` or `host:port` of the actor URL.
    pub domain: String,
    pub display_name: String,
    pub note: String,
    pub locked: bool,
    pub inbox_uri: String,
    pub shared_inbox_uri: Option<String>,
    pub public_key_pem: String,
    pub key_id: String,
}

/// The one state change an inbound activity causes.
#[derive(Debug, Clone)]
pub enum InboundEffect {
    /// The remote actor follows the local `target_id`. An unlocked target
    /// accepts at once, which enqueues `accept`. A locked target keeps a
    /// pending request, and `accept` is dropped.
    Follow {
        target_id: Id,
        follow_uri: String,
        accept: DeliverySpec,
    },
    /// The remote actor accepts our outbound follow `follow_uri`.
    Accept { follow_uri: String },
    /// The remote actor rejects our outbound follow `follow_uri`: the pending row goes.
    Reject { follow_uri: String },
    /// The remote actor undoes its follow of the local `target_id`.
    UndoFollow { target_id: Id },
}

/// The result of committing an inbound activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundDisposition {
    /// The effect was applied and its jobs were committed.
    Applied,
    /// The activity ID was already recorded. Nothing changed.
    Duplicate,
    /// The activity was recorded, but it had no effect. The reason is logged,
    /// for example an Accept that matches no pending follow.
    Ignored(String),
}

/// Persistence for federation: keys, remote accounts, the activity dedup
/// record, and the inbound and outbound follow commands.
pub trait FederationStore: Send + Sync {
    /// The keys of an account. Local accounts also carry the private key.
    fn actor_keys(&self, account_id: Id) -> Result<Option<ActorKeys>>;

    /// Store a generated local key pair if the account has none yet. The first
    /// writer wins, and the stored pair is returned, so concurrent generation is harmless.
    fn store_local_keys(
        &self,
        account_id: Id,
        key_id: &str,
        public_key_pem: &str,
        private_key_pem: &str,
    ) -> Result<ActorKeys>;

    /// The account with this actor URI, local or remote.
    fn account_by_actor_uri(&self, actor_uri: &str) -> Result<Option<Account>>;

    /// Whether an activity with this canonical URI was already applied.
    fn activity_seen(&self, activity_uri: &str) -> Result<bool>;

    /// Delivery health of a destination host (`None` = no failures on record).
    fn delivery_dest(&self, host: &str) -> Result<Option<DeliveryDestState>>;

    /// Store the delivery health of a destination host. One transaction, no
    /// network I/O. The caller computes the state (see
    /// `federation::delivery`); a success resets it to zero failures.
    fn set_delivery_dest(&self, host: &str, state: DeliveryDestState) -> Result<()>;

    /// Upsert the remote account, record the activity, apply its effect and
    /// commit the follow-up jobs, all in ONE transaction. A duplicate activity
    /// commits nothing.
    fn commit_inbound(
        &self,
        activity_uri: &str,
        actor: RemoteAccountUpsert,
        effect: InboundEffect,
        now_ms: u64,
    ) -> Result<InboundDisposition>;

    /// Record an outbound follow request (pending until the remote accepts) and
    /// its `DeliverActivity` job, in ONE transaction.
    fn request_follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        follow_uri: &str,
        delivery: DeliverySpec,
        now_ms: u64,
    ) -> Result<Follow>;

    /// The follow row that carries this outbound follow URI, if any.
    fn follow_by_uri(&self, follow_uri: &str) -> Result<Option<Follow>>;

    /// Upsert a remote account from a resolved actor document (no activity).
    fn upsert_remote(&self, actor: RemoteAccountUpsert, now_ms: u64) -> Result<Account>;

    /// Remove a follow (accepted or pending) and enqueue the Undo delivery, in
    /// ONE transaction. Returns the removed row, or `None` when there was none.
    fn undo_follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        delivery: DeliverySpec,
        now_ms: u64,
    ) -> Result<Option<Follow>>;
}

/// A report to store (an inbound Flag, or a local one).
#[derive(Debug, Clone)]
pub struct NewReport {
    pub reporter_id: Id,
    pub target_account_id: Id,
    pub status_ids: Vec<Id>,
    pub comment: String,
    pub forwarded: bool,
    pub activity_uri: Option<String>,
}

/// The content side of federation: statuses by URI, quotes, reply links,
/// reports, remote media, and the thread-refresh cooldown. Each mutation commits
/// in one transaction with the federation event job it causes, when there is one.
pub trait FederationContentStore: Send + Sync {
    /// A status by its URI, local or remote.
    fn status_by_uri(&self, uri: &str) -> Result<Option<Status>>;

    /// Record an applied inbound activity's URI. Called after its effect, so an
    /// ignored activity is never recorded (D33).
    fn record_activity(&self, activity_uri: &str, now_ms: u64) -> Result<()>;

    /// Whether an activity URI is recorded as applied.
    fn activity_recorded(&self, activity_uri: &str) -> Result<bool>;

    /// File a tombstone for an object URI a remote `Delete` removed before any
    /// `Create` for it arrived. Returns `true` when the tombstone is new.
    /// One transaction, no network I/O.
    fn record_remote_tombstone(&self, uri: &str, now_ms: u64) -> Result<bool>;

    /// When the tombstone for `uri` was filed, if it was.
    fn remote_tombstone(&self, uri: &str) -> Result<Option<u64>>;

    /// A quote by the authorization URI it carries (`approval_uri`).
    fn quote_by_approval_uri(&self, approval_uri: &str) -> Result<Option<QuoteRecord>>;

    /// A quote by the status that quotes.
    fn quote_by_quoting(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>>;

    /// A quote by its ID.
    fn quote_by_id(&self, quote_id: Id) -> Result<Option<QuoteRecord>>;

    /// A report by its ID.
    fn report_by_id(&self, report_id: Id) -> Result<Option<Report>>;

    /// Create a quote of `quoted_status_id` by `quoting_status_id` in `state`.
    /// An accepted quote counts on the quoted status; a pending one does not.
    fn create_quote(&self, input: NewQuote, now_ms: u64) -> Result<QuoteRecord>;

    /// Move a quote to `state` with its authorization URI, keeping the quoted
    /// status's count in step with "accepted".
    fn set_quote_state(
        &self,
        quote_id: Id,
        state: QuoteState,
        approval_uri: Option<String>,
        quote_activity_uri: Option<String>,
    ) -> Result<QuoteRecord>;

    /// Link a reply to its parent once the parent is known (thread completion).
    /// Idempotent. Never changes a reply into a root. Clears the unresolved parent.
    fn link_reply(&self, child_id: Id, parent_id: Id) -> Result<()>;

    /// Remember (or clear, with `None`) the parent URI of a reply whose parent is unknown.
    fn set_unresolved_parent(&self, status_id: Id, parent_uri: Option<&str>) -> Result<()>;

    /// The parent URI of a reply whose parent is still unknown here.
    fn unresolved_parent(&self, status_id: Id) -> Result<Option<String>>;

    /// Local statuses of an account, newest first, for account deletion (a bounded page).
    fn account_status_ids(&self, account_id: Id) -> Result<Vec<Id>>;

    /// The last thread refresh of a status, if any.
    fn remote_refresh(&self, status_id: Id) -> Result<Option<RefreshRecord>>;

    /// Record a thread refresh of a status.
    fn set_remote_refresh(&self, status_id: Id, record: RefreshRecord) -> Result<()>;

    /// Store a report. A local report forwarded to the target's server also
    /// enqueues its Flag, in the same transaction.
    fn insert_report(&self, input: NewReport, now_ms: u64) -> Result<Report>;

    /// Enqueue a federation event for a local actor (an inbound activity's
    /// response, such as an Accept of a quote request). A remote actor is a no-op.
    fn enqueue_event(&self, actor_id: Id, event: serde_json::Value, now_ms: u64) -> Result<()>;

    /// Remember where a remote attachment's bytes come from.
    fn set_remote_media_source(&self, media_id: Id, url: &str) -> Result<()>;

    /// The source URL of a remote attachment.
    fn remote_media_source(&self, media_id: Id) -> Result<Option<String>>;

    /// Attach fetched bytes to a remote attachment and enqueue its processing,
    /// in one transaction. Returns the row, or `None` if it does not exist.
    fn stage_remote_media(
        &self,
        media_id: Id,
        file_key: &str,
        mime_type: &str,
        file_size: u64,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;

    /// Mark a remote attachment as permanently failed.
    fn mark_remote_media_failed(&self, media_id: Id, reason: &str, now_ms: u64) -> Result<()>;
}
/// A quote to create.
#[derive(Debug, Clone)]
pub struct NewQuote {
    pub quoting_status_id: Id,
    pub quoted_status_id: Id,
    pub quoting_account_id: Id,
    pub quoted_account_id: Id,
    pub state: QuoteState,
    pub approval_uri: Option<String>,
    pub quote_activity_uri: Option<String>,
}
