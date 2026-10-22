# 2026-10-22 — S2 router split gate record (supersedes WIP `s2-router-split.md`)

Supersedes the WIP backup log `state/logs/2026-10-22-s2-router-split.md`
(committed red for backup during the disk-full incident). Code unchanged
since `2d9a969`; this file records the completed gate.

## Build environment (new)
- `Cargo.toml` `[profile.dev]` + `[profile.test]`: `debug =
  "line-tables-only"`, `incremental = false`.
- `birchtree/.cargo/config.toml` (`target-dir =
  "C:/Users/julie/.cargo/shared-target"`, gitignored, machine-local,
  shared with pinetree — build one tree at a time).
- Toolchain: `stable-x86_64-pc-windows-gnu` + WinLibs `gcc`/`dlltool` on
  PATH (native Windows, no WSL). Fresh full rebuild into the shared dir.

## Gate results
- `cargo check --all-targets`: exit 0. No warnings from `routes/` or
  `router.rs`; remaining warnings pre-existing (`transcode.rs:pid`,
  `fix_*.rs` / `adv2_*` test imports).
- `cargo test` (full): **1136 passed, 0 failed** (1133 pre-existing incl.
  `cargo test --lib` 242/242, plus 3 new `router_order` tests).
- Route audit: 246 `.route(` calls across the 13 fragments (matches the
  pre-split count); no cross-file path dupes; static-before-`:id` order
  preserved intra-file and proven by
  `verify_credentials_resolves_before_id` (401 vs 404).
- `rustfmt --edition 2024 --check` on parent-touched files: clean.
  Tree-wide `cargo fmt` deliberately not run (pre-existing fmt drift under
  rustfmt 1.99). `scripts/smoke-all.sh` not run (requires bash/WSL;
  native-Windows rule).
- Shared target size after full build: ~24 GB (was ~78 GB local).

## Next
S2 done. Suggested order per proposal: S3 (main/cli/bootstrap) → S9
(config split) → parallel S4|S6|S7|S8|S11 → S5 → S10 → S12. Delegate to
subagents with disjoint file sets; parent owns shared files and gates.
