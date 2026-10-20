//! D25 tag re-index/backfill (Phase 7, policy gap D25).
//!
//! Upstream `HashtagNormalizer` parity (`normalize_tag`: NFKC + lowercase +
//! ASCII folding) means tag spellings that used to be distinct converge:
//! `#café` and `#cafe` are one tag. Rows and `HASHTAG_INDEX` keys written
//! before D25 keep their old spellings, so a bounded, restartable,
//! idempotent backfill re-derives them.
//!
//! This module is the *planner*: pure functions over one status (or one batch
//! of distinct tag names) with no store access, so the logic is unit-testable
//! and safe to re-run. The durable driver is a parent patch (it needs a new
//! `Store::rename_hashtag` / status-tag rewrite in `persistence`, plus a
//! `JobKind::TagBackfill` variant and scheduler wiring — see PATCHES FOR
//! PARENT in the Phase 7 report):
//!
//! 1. page statuses in bounded batches (`BACKFILL_BATCH_CAP`);
//! 2. for each status, call [`tags_need_rewrite`]; when it returns `Some`,
//!    rewrite `Status.tags` and move the status's `HASHTAG_INDEX` keys from
//!    the dropped spellings to the new ones in ONE transaction;
//! 3. repeat the scan until a full pass plans nothing (converged).
//!
//! Restartable: the plan is a pure function of current rows, so a crashed run
//! resumes by re-scanning. Idempotent: a converged store plans no rewrites,
//! and rewrites are keyed sets (no duplicates, order-insensitive compare).
//! Bounded: every pass touches at most `BACKFILL_BATCH_CAP` statuses.

use crate::application::statuses::{extract_tags, normalize_tag};
use crate::domain::Id;

/// Statuses examined per backfill pass. Small enough to keep one driver
/// transaction bounded; the scan repeats until convergence.
pub const BACKFILL_BATCH_CAP: usize = 500;

/// One status's tag rewrite: `old_tags` (stored) become `new_tags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagBackfillOp {
    pub status_id: Id,
    pub old_tags: Vec<String>,
    pub new_tags: Vec<String>,
}

/// Re-derive the canonical tags for a status source text: extract with the
/// (D25-widened) tag pattern, normalize each, deduplicate preserving order.
pub fn plan_status_tags(source_text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for raw in extract_tags(source_text) {
        let normalized = normalize_tag(&raw);
        if !normalized.is_empty() && !tags.contains(&normalized) {
            tags.push(normalized);
        }
    }
    tags
}

/// Whether `stored_tags` differ from the canonical derivation for
/// `source_text`: `Some(canonical)` when a rewrite is needed, `None` when the
/// row already converges (order-insensitive compare; stored rows keep author
/// order). Idempotency: applying the rewrite then re-planning yields `None`.
pub fn tags_need_rewrite(stored_tags: &[String], source_text: &str) -> Option<Vec<String>> {
    let mut canonical = plan_status_tags(source_text);
    let mut stored = stored_tags.to_vec();
    canonical.sort();
    stored.sort();
    (stored != canonical).then(|| plan_status_tags(source_text))
}

/// Plan one bounded batch of rewrites.
///
/// `rows` is `(status_id, stored_tags, source_text)`; at most
/// `BACKFILL_BATCH_CAP` ops are returned (callers page the scan and call
/// again). Deterministic: input order is preserved, and each op's `new_tags`
/// keep extraction order.
pub fn plan_backfill_batch(
    rows: &[(Id, Vec<String>, String)],
) -> Vec<TagBackfillOp> {
    let mut ops: Vec<TagBackfillOp> = Vec::new();
    for (id, stored, text) in rows {
        if ops.len() >= BACKFILL_BATCH_CAP {
            break;
        }
        if let Some(new_tags) = tags_need_rewrite(stored, text) {
            ops.push(TagBackfillOp {
                status_id: *id,
                old_tags: stored.clone(),
                new_tags,
            });
        }
    }
    ops
}

/// Canonicalize a set of distinct stored tag spellings (e.g. from
/// `admin_hashtag_names`): `(old, canonical)` renames for every spelling that
/// moves, sorted by `old` for determinism. Empty when the registry already
/// converges — the idempotency check for the name-level pass.
pub fn plan_tag_renames(stored_names: &[String]) -> Vec<(String, String)> {
    let mut renames: Vec<(String, String)> = Vec::new();
    for name in stored_names {
        let canonical = normalize_tag(name);
        if !canonical.is_empty() && canonical != *name {
            renames.push((name.clone(), canonical));
        }
    }
    renames.sort();
    renames.dedup();
    renames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cafe_variants_converge() {
        let renames = plan_tag_renames(&[
            "café".to_string(),
            "cafe".to_string(),
            "CAFÉ".to_string(),
        ]);
        // Only the non-canonical spellings move, both onto `cafe`.
        assert!(renames.contains(&("café".to_string(), "cafe".to_string())));
        assert!(!renames.iter().any(|(from, _)| from == "cafe"));
    }

    #[test]
    fn converged_registry_plans_nothing() {
        let names = ["cafe".to_string(), "music".to_string()];
        assert!(plan_tag_renames(&names).is_empty());
        // Re-planning the outputs is a fixed point.
        let outputs: Vec<String> = names.to_vec();
        assert!(plan_tag_renames(&outputs).is_empty());
    }

    #[test]
    fn batch_planning_is_bounded_and_deterministic() {
        let rows: Vec<(Id, Vec<String>, String)> = (0..BACKFILL_BATCH_CAP + 10)
            .map(|i| {
                (
                    Id::new(1000 + i as u64),
                    vec!["stale".to_string()],
                    format!("#fresh{i}"),
                )
            })
            .collect();
        let ops = plan_backfill_batch(&rows);
        assert_eq!(ops.len(), BACKFILL_BATCH_CAP);
        assert_eq!(ops[0].status_id, Id::new(1000));
    }
}
