//! Bounded inbound-activity dedup (Phase 6).
//!
//! The inbox records an activity URI after it applies (or replays as a
//! duplicate) so a redelivery is a cheap duplicate-never-ignored read
//! (D33: an *ignored* activity is never recorded, so a redelivery is
//! re-evaluated instead of cementing the ignore — needed because Mastodon
//! reuses an Undo's ID for a later undo of the same follow row).
//!
//! Upstream keeps no equivalent global activity-ID table (its dedup is the
//! unique status URI plus short-lived redis markers such as the 6-hour
//! `delete_upon_arrival` flag), so the retention below is this server's
//! (D33 accepted the record-applied-only rule; this module bounds the
//! table). Entries live [`DEDUP_TTL_MS`]: long enough to cover every
//! redelivery tail (delivery retries span hours; remote sidekiq tails span
//! days), short enough that the table cannot grow without bound. A
//! redelivery past the window simply re-evaluates the activity — the safe
//! default, since every receive path is idempotent.
//!
//! Pruning itself needs a store method (`prune_activities_before`), which
//! lives in the parent-owned persistence layer; this module owns the policy
//! (the TTL and the cutoff) and the D33 rule statement.

/// How long a recorded activity URI is remembered: 7 days in milliseconds.
/// Covers the longest redelivery tails (outbound sidekiq tails run ~days)
/// with margin; past it a redelivery re-evaluates idempotently.
pub const DEDUP_TTL_MS: u64 = 7 * 86_400_000;

/// The oldest `recorded_at_ms` a pruner keeps: everything at or below
/// `now_ms - DEDUP_TTL_MS` may be dropped.
pub fn prune_cutoff_ms(now_ms: u64) -> u64 {
    now_ms.saturating_sub(DEDUP_TTL_MS)
}

/// Whether a dedup entry recorded at `recorded_at_ms` is still live at
/// `now_ms` (a `now_ms` in the past keeps everything: fail closed).
pub fn entry_live(recorded_at_ms: u64, now_ms: u64) -> bool {
    recorded_at_ms >= prune_cutoff_ms(now_ms)
}
