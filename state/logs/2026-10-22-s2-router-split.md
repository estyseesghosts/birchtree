# 2026-10-22 — S2 router split (WIP backup commit, NOT gated)

Mid-flight backup: full `cargo test` could not run (C: ~395 MB free;
link fails `No space left on device`). Committed red on purpose as a
backup; the next implementer must free disk and run the gate.

## What is in this commit
- `src/api/mastodon/routes/` (13 `routes_*.rs` fragments + `mod.rs`, 246
  `.route(` calls, no cross-file path dupes).
- `src/api/mastodon/router.rs` rewritten as merges; `mod.rs` gains
  `pub mod routes;`.
- `tests/router_order.rs` (3 tests: fragment-count snapshot,
  verify_credentials-before-`:id`, `/health`).
- Parent merge fixes: `/auth/password` only in `routes_oauth.rs`;
  `/timelines/link` only in `routes_timelines.rs`; orphans 244–251
  restored in `routes_collections.rs`; unused verb imports narrowed.

## Verified before disk filled up
- `cargo check --all-targets`: exit 0, no warnings from new files.
- `cargo test --test router_order`: 3 passed.

## Still required
Free disk (target/ ~78 GB, %TEMP% ~135 GB), then `cargo test` (full),
then replace this log body with the real gate record per the proposal.
`scripts/smoke-all.sh` needs bash/WSL — not run (native-Windows rule).
