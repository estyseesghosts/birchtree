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
    /// Placeholder for the media-processing pipeline (Phase 4).
    ProcessMedia,
    /// Placeholder for outbound federation delivery (Phase 6).
    DeliverActivity,
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
            JobKind::DeliverActivity => "deliver_activity",
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
