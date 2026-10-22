# 2026-10-22 — S3 main/cli/bootstrap split (decomposition proposal)

## What changed
S3 per proposal: `src/main.rs` (1300 lines) split into:
- `src/cli.rs` (new, ~300 lines) — `Args` (now `pub(crate)` with
  `pub(crate)` fields) + `parse_args` verbatim. Testability without
  behavior change: parsing of `--max-body-size`, `--max-status-chars`,
  `--trusted-proxy` factored into pure `pub(crate)` helpers returning the
  original message as `Err(String)`; `parse_args` prints + `exit(2)` exactly
  as before. 9 `#[cfg(test)]` unit tests assert exact strings + valid inputs.
- `src/bootstrap.rs` (new, 296 lines) — `load_secret_box_or_exit`,
  `open_blob_pools` (returns the triple; caller wraps `Arc`), 
  `restore_cache_ledger` (returns `maint`), `build_app_state` (async,
  returns `ApiState` only; probes/router stay in `serve`). Bodies verbatim.
- `src/main.rs` (1300 → 884 lines) — `mod bootstrap; mod cli;` +
  `use cli::{Args, parse_args};`; `serve` now reads
  secrets → layout → pools → ledger → state → router → worker as calls.
  `load_secret_box`, `Args`, `parse_args` deleted from main; unused imports
  (`ApiState`, `ServerInfo`, `Context`) pruned.
- `serve` reconstructs post-state handles without behavior change:
  `streaming` from `state.streams.clone()` (`Hub: Clone`),
  `federation` from `state.federation.clone()`; `blobs.clone()` passed
  down (still owned for the worker); `maint` from the ledger call.

Delegation: 2 subagents (disjoint spans, new-files-only); parent cutover
+ one fix: `maint.seed_jobs(&**store, ...)` (double deref — `store` is
`&Arc<RedbStore>` in the new signature vs `Arc<RedbStore>` inline before).
`rustfmt` clean on all touched files (file-scoped; no tree-wide fmt).

Out of scope (noted, not done): `run_*` admin commands and the probes
block stay in `main.rs` (future slice material); `serve` is 258 lines
including probes/worker/mailer/listener.

## Verification (native Windows, shared target)
- `cargo check --all-targets`: exit 0, zero new warnings (remaining:
  pre-existing `transcode.rs:pid`, `fix_*.rs`/`adv2_*` test imports).
- `cargo test --bin birchtree cli`: 9 passed, 0 failed.
- `cargo test` (full): 1153 passed, 0 failed.
