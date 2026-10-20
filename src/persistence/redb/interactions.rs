//! Interaction-event journal (D39 `interactions` measure).
//!
//! One row per local interaction in [`INTERACTION_EVENTS`](super::schema::INTERACTION_EVENTS)
//! (keyed by event id -> JSON [`InteractionEvent`]). Writers are the
//! application use cases (`statuses::create_status` for non-self replies,
//! `relationships::follow` for new follows/requests, `polls::vote` for cast
//! ballots; favourite/reblog writers land in `application::interactions` via
//! the parent snippet); the reader is the admin `interactions` measure.
//!
//! Id policy: the row id is minted here via `next_id()` (timestamp-oriented,
//! like every other store write) and the caller-supplied `event.id` is
//! replaced — application callers hold no `IdGenerator`. Journal-once
//! discipline lives with the callers (skip replays/no-ops); a retried *job*
//! must never call this.
//!
//! Window reads: event ids embed the mint millisecond in the high 48 bits
//! (`Id::from_parts`), so `[since_ms, until_ms)` maps to a bounded id range
//! scan; each row is then filtered by its authoritative `created_at_ms`.
//!
//! Soundness invariant (holds by construction: every writer passes the live
//! clock's `now_ms` and the id is minted from the same clock microseconds
//! later, so mint time >= `created_at_ms`): any event with
//! `created_at_ms >= since_ms` has an id `>= since_ms << 16`, so the lower
//! id bound never drops an in-window row. The upper bound can only miss a
//! row whose write stalled past `until_ms` — impossible for callers that
//! query with a live `until` (the measure always does). The `created_at_ms`
//! filter is exact, so out-of-window rows that fall inside the id range
//! (same-ms neighbours) never leak in. Callers must NOT query with an
//! `until_ms` within a millisecond of the write clock (a ticking clock mints
//! ids past a razor-thin bound); the measure's day buckets always carry
//! whole-day slack.

use crate::application::ports::InteractionLog;
use crate::domain::{InteractionEvent, InteractionEventKind, Result};

use super::schema::INTERACTION_EVENTS;
use super::{RedbStore, decode, encode};

/// Hard bound on rows walked per count call (same bound as the other admin
/// metric scans).
const INTERACTION_SCAN_CAP: usize = 200_000;

impl InteractionLog for RedbStore {
    fn record_interaction(&self, event: InteractionEvent) -> Result<()> {
        let write = self.begin_write()?;
        let stored = InteractionEvent {
            id: self.next_id(),
            account_id: event.account_id,
            kind: event.kind,
            created_at_ms: event.created_at_ms,
        };
        {
            let mut table = write.open_table(INTERACTION_EVENTS)?;
            table.insert(stored.id.value(), encode(&stored)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn count_interactions(
        &self,
        kind: Option<InteractionEventKind>,
        since_ms: u64,
        until_ms: u64,
    ) -> Result<u64> {
        if since_ms >= until_ms {
            return Ok(0);
        }
        // Timestamp-oriented ids sort by mint time: bound the scan to the
        // window's id range, then filter by the authoritative `created_at_ms`.
        let lower = since_ms.checked_shl(16).unwrap_or(u64::MAX);
        let upper = until_ms.checked_shl(16).unwrap_or(u64::MAX);
        let read = self.begin_read()?;
        let table = read.open_table(INTERACTION_EVENTS)?;
        let mut count = 0u64;
        let mut scanned = 0usize;
        for entry in table.range(lower..upper)? {
            let (_, value) = entry?;
            scanned += 1;
            if scanned > INTERACTION_SCAN_CAP {
                break;
            }
            let event: InteractionEvent = decode(value.value())?;
            if event.created_at_ms < since_ms || event.created_at_ms >= until_ms {
                continue;
            }
            if kind.is_some_and(|wanted| wanted != event.kind) {
                continue;
            }
            count += 1;
        }
        self.note_scanned(scanned);
        Ok(count)
    }
}
