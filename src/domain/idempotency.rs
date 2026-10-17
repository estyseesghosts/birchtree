//! Idempotency records for retried POSTs (Mastodon `Idempotency-Key`).
//!
//! A record is committed in the same transaction as the mutation it protects,
//! so a retry can never observe a mutation without its idempotency record (or
//! vice versa). Retention is one hour, matching Mastodon's `SETEX 3600`.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Retention window for idempotency records (1 hour, per Mastodon).
pub const IDEMPOTENCY_TTL_MS: u64 = 3_600_000;

/// What a repeated request replays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    /// When the original request committed.
    pub created_at_ms: u64,
    /// The operation the key protects (e.g. `status_create`).
    pub scope: String,
    /// Id of the originally created entity.
    pub result_id: Id,
}

impl IdempotencyRecord {
    /// True when a lookup at `now_ms` should still replay this record.
    pub fn is_fresh_at(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) < IDEMPOTENCY_TTL_MS
    }
}
