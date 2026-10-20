//! Durable job envelope.
//!
//! A job is committed in the *same* transaction as the state mutation that
//! requires it (see `birchtree.md` §5), so a committed mutation can never lose
//! its follow-up work. Network I/O, media processing, notification fan-out, and
//! federation delivery all run as jobs, after the commit.
//!
//! This is the canonical envelope. `jobs::types` re-exports it; the queue and
//! worker live under `jobs/`.

use crate::domain::Id;

/// What a job is for. Only the types needed by the current phase are defined;
/// new variants are added as their phase lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Emitted atomically with local account creation. Phase 1 uses it to prove
    /// commit-with-job durability; later phases deliver a welcome/mail step.
    UserCreated,
    /// Send an account email-confirmation message through the `MailSender`
    /// adapter. Enqueued atomically with public registration; never sent inside
    /// the request transaction.
    SendConfirmationEmail,
    /// Create a notification record for the recipient, if not already present
    /// (deduplicated through the notification dedup index, so a replayed or
    /// retried job never duplicates a notification). Enqueued in the same
    /// transaction as the interaction that produced it.
    FanOutNotification,
    /// Mark a poll expired once its expiry elapses. Enqueued in the same
    /// transaction as the poll's status; the handler is idempotent.
    PollExpired,
    /// Finish processing an uploaded attachment whose bytes are already
    /// staged (decode/preview/blurhash). Enqueued in the same transaction as
    /// the attachment's metadata; the handler is idempotent.
    ProcessMedia,
    /// Publish a due scheduled status: destroy the row and replay its params
    /// through the normal status-creation use case. Enqueued in the same
    /// transaction as the scheduled row; a replay after deletion is a no-op.
    PublishScheduledStatus,
    /// Placeholder for outbound federation delivery (Phase 6).
    DeliverActivity,
    /// A local change that federates (status, like, boost, block, quote, flag).
    /// Enqueued in the same transaction as the change; the worker builds the
    /// activity and enqueues one `DeliverActivity` per destination inbox.
    FederationEvent,
    /// Fetch the parent of a remote reply whose parent is unknown (Phase 6,
    /// thread completion). One hop per job; the handler is idempotent.
    ResolveParent,
    /// Crawl a remote status's `replies` collection (Phase 6, thread completion).
    CrawlReplies,
    /// Fetch a remote attachment's bytes and stage them for processing (Phase 6).
    FetchRemoteMedia,
    /// Generate an annual report for one account and year (Phase 7.3). The
    /// handler computes real stats from local records and inserts the row
    /// idempotently; a retry after commit is a no-op.
    GenerateAnnualReport,
    /// Re-index (or remove) one status in the embedded search index
    /// (Phase 7.2). Create/edit/delete/visibility-change all converge here;
    /// the handler is idempotent.
    SearchIndexStatus,
    /// One bounded batch of the search backfill (Phase 7.2). The payload
    /// carries the resume cursor; each batch enqueues its follow-up.
    SearchBackfill,
    /// Run the Phase 7.1 ffmpeg transcode for one attachment.
    TranscodeMedia,
    /// Deliver one Web Push tickle (Phase 7):
    /// `{subscription_id, notification_id}`.
    WebPushDelivery,
    /// One bounded batch of the tag-normalization backfill (Phase 7, D25).
    /// The payload carries the resume cursor; each batch enqueues its
    /// follow-up until a pass plans nothing.
    TagBackfill,
    /// One bounded batch of the async account-deletion worker (Phase 7.3,
    /// D38). The payload carries `{account_id, after}` where `after` is the
    /// exclusive resume cursor (`None` starts at the newest status). Each
    /// batch tombstones at most one page of the account's statuses (each
    /// tombstone commits its own federated `Delete` event) and enqueues its
    /// follow-up while a full page was seen. A crashed batch is retried from
    /// its own cursor; tombstones are idempotent, so replays are harmless.
    AccountDeletionBatch,
    /// Fetch one trend link's preview-card metadata through the safe HTTP
    /// client (Phase 7.3, D40). The payload carries `{link_id, url}`. The
    /// handler is idempotent (a row that already has metadata is a no-op)
    /// and never fabricates card fields.
    PreviewCardFetch,
}

impl JobKind {
    /// Stable string key used in stored payloads and logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            JobKind::UserCreated => "user_created",
            JobKind::SendConfirmationEmail => "send_confirmation_email",
            JobKind::FanOutNotification => "fan_out_notification",
            JobKind::PollExpired => "poll_expired",
            JobKind::ProcessMedia => "process_media",
            JobKind::PublishScheduledStatus => "publish_scheduled_status",
            JobKind::DeliverActivity => "deliver_activity",
            JobKind::FederationEvent => "federation_event",
            JobKind::ResolveParent => "resolve_parent",
            JobKind::CrawlReplies => "crawl_replies",
            JobKind::FetchRemoteMedia => "fetch_remote_media",
            JobKind::GenerateAnnualReport => "generate_annual_report",
            JobKind::SearchIndexStatus => "search_index_status",
            JobKind::SearchBackfill => "search_backfill",
            JobKind::TranscodeMedia => "transcode_media",
            JobKind::WebPushDelivery => "web_push_delivery",
            JobKind::TagBackfill => "tag_backfill",
            JobKind::AccountDeletionBatch => "account_deletion_batch",
            JobKind::PreviewCardFetch => "preview_card_fetch",
        }
    }
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting to be claimed (due at `due_at_ms`).
    Pending,
    /// Claimed by a worker under a lease (reclaimable after `lease_until_ms`).
    Leased,
    /// Finished successfully.
    Succeeded,
    /// Permanently failed and quarantined for operator inspection.
    Quarantined,
}

impl JobState {
    /// Whether a job in this state still needs index/queue entries.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Succeeded | JobState::Quarantined)
    }
}

/// A durable job record.
///
/// `due_at_ms` is the time the job becomes claimable. While leased, the queue
/// index is keyed by `lease_until_ms` so a crashed worker's job becomes
/// claimable again automatically after the lease elapses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: Id,
    pub kind: JobKind,
    /// Opaque, job-kind-specific payload (JSON object).
    pub payload: serde_json::Value,
    pub state: JobState,
    /// Claimable time, or lease expiry while `Leased`.
    pub due_at_ms: u64,
    /// Lease expiry while `Leased`; `None` otherwise.
    pub lease_until_ms: Option<u64>,
    /// Number of times the job has been claimed.
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
    pub created_at_ms: u64,
}

impl Job {
    /// Default retry budget for a job.
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;
    /// Build a new pending job.
    pub fn new(
        id: Id,
        kind: JobKind,
        payload: serde_json::Value,
        due_at_ms: u64,
        created_at_ms: u64,
    ) -> Self {
        Self {
            id,
            kind,
            payload,
            state: JobState::Pending,
            due_at_ms,
            lease_until_ms: None,
            attempts: 0,
            max_attempts: Self::DEFAULT_MAX_ATTEMPTS,
            last_error: None,
            created_at_ms,
        }
    }
}

/// How long a worker may hold a claim before the job is reclaimable.
pub const LEASE_MS: u64 = 30_000;
/// Base retry backoff.
pub const BACKOFF_BASE_MS: u64 = 1_000;
/// Maximum retry backoff.
pub const BACKOFF_CAP_MS: u64 = 300_000;

/// Exponential backoff for the given attempt count (1-based).
pub fn backoff_ms(attempts: u32) -> u64 {
    let shift = attempts.saturating_sub(1).min(8);
    (BACKOFF_BASE_MS << shift).min(BACKOFF_CAP_MS)
}
