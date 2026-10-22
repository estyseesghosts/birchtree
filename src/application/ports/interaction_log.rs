use crate::domain::{Error, InteractionEvent, InteractionEventKind, Result};

/// Unified interaction-event journal backing the admin `interactions`
/// measure (D39). Writers are the application use cases, one row per local
/// interaction; readers are the admin measures. Scaffold defaults refuse
/// loudly; RedbStore overrides (see `persistence::redb::interactions`).
pub trait InteractionLog: Send + Sync {
    /// Append one interaction event (idempotent only by construction: callers
    /// journal each user action once; a retried *job* must not re-journal —
    /// journal at the use-case commit, never in a job handler).
    fn record_interaction(&self, event: InteractionEvent) -> Result<()> {
        let _ = event;
        Err(Error::Internal(
            "record_interaction is not implemented".into(),
        ))
    }

    /// Count journaled events in `[since_ms, until_ms)`, optionally filtered
    /// to one kind. `None` kind counts everything (the `interactions` key).
    fn count_interactions(
        &self,
        _kind: Option<InteractionEventKind>,
        _since_ms: u64,
        _until_ms: u64,
    ) -> Result<u64> {
        Err(Error::Internal(
            "count_interactions is not implemented".into(),
        ))
    }
}
