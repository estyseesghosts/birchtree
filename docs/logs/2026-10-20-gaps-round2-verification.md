# 2026-10-20 — gap-closing round 2 verification (D6/D15/D20/D21/D23/D24/D28/D36–D40/D44 + federation edges + minors)

## Gate

- `cargo check -j 2 --all-targets`: clean, zero warnings, zero errors.
- `cargo test -j 2 --release`: **542 passed, 0 failed** across 42 suites (baseline on `f9c2c7c` was 494/494; +44 new round-2 tests, +4 from the in-flight tag work present in the tree).
- New suites, all green in release: `phase7_gaps_round2_a` 9/9, `phase7_gaps_round2_b` 8/9 (1 ignored live-measurement), `phase7_gaps_round2_c` 4/4, `phase7_gaps_round2_d` 12/12, `phase6_gaps_round2_e` 11/11.
- Regressions re-verified: `phase5_streaming` 31, `phase4_timelines` 23, `phase7_policy_gaps` 17, `phase6_federation` 17, `phase6_content` 32, `phase7_3_admin_*` suites, `phase2_api`, `phase3_relationships`, `phase4_1_json_parity`.
- HTTP smoke: `smoke-phase7-3.sh` OK, `smoke-phase5.sh` OK (incl. revocation-closes-stream), `smoke-phase7.sh` OK.

## Measurements (D15)

- 605 retractable statuses → 2 batches, 2 ms; 10k → 21 batches, 49 ms, `truncated=false`. No latency escalation needed. Bound: 64 batches × 500 matches = 32k statuses.

## Register/manifest deltas (all applied)

- `docs/compatibility-profile.md`: D6 (deploy-time switch), D15→implemented, D20→decided planned-gap, D21→implemented, D23 clear path, D24→implemented, D28 GET pass-through, D36→implemented, D37→implemented, D38→implemented, D39→partly implemented, D40→implemented, new D44 (web push-destroy numeric-id CSRF), open-decisions D6 bullet.
- `docs/endpoint-manifest.md`: web push-destroy auth cell, follow `languages[]`, search deploy-time + backfill bullet, update_credentials chosen_languages, admin DELETE/action/enable/unsuspend/approve/reject, reports rule_ids, measures/dimensions/retention keys, trends score-ordered rows, streaming chosen_languages note.
- `docs/streaming-contract.md`: retraction bound, token-kill bullet, planned-gap bullet, bounds table, public/hashtag gate rows, deviations D15/D20/D21/D24.
- `docs/federation-contract.md`: per-destination budget/cooldown, Delete-beats-Create tombstones, doc-fetch limits, D38 local delete distribution.
- `docs/phase-6-exit-criteria.md`: 4xx row corrected, cooldown row and doc-validation row checked.

## Schema

- v10 (E: `remote_tombstone`, `delivery_dest`) + v11 (C: `rules`, `account_warnings`, `sign_ins`), sequential, `SCHEMA_VERSION = 11`. Tables-only steps, no backfill.

## Collision notes

- The 4 in-flight tag-work files (`activitypub/wire.rs`, `application/federation_inbound.rs`, `application/statuses.rs`, `persistence/redb/statuses.rs`) contain zero round-2 markers (verified by grep); subagents never touched them.
- Subagent B made 3 compile-unblock fixes in C's lane (`approved: input.approved`, `..Default::default()` on rewritten `approve`, `text` passthrough) — reviewed at merge, kept.
- D40 row references D21 as implemented (chosen_languages shipped by B); C's D39 still refuses tag telemetry keys (7-day history only — documented).
- Media flakiness: verify-only, no code (E). No success-shaped stubs: refused metric keys stay refused with reasons; unshipped pieces stay empty/404.
