# 2026-10-22 — S0 shared param/context objects (decomposition proposal)

## What changed
S0 per `birchtree-decomposition-proposal.md` (foundations; no behavior change).
New files (additive only, no signature deletions):
- `src/application/page.rs` — `PageCursor { max_id, since_id, min_id, limit }` + `new`/`clamp`, `AscendingPage { cursor, ascending }`.
- `src/application/search_query.rs` — `SearchQuery<'a>` (9 fields mirroring `search()` arg order) + `new`.
- `src/application/admin_page.rs` — `AdminPage` + `new`/`clamp`, generic `AdminFilterPage<F>`.
- `src/application/media_input.rs` — canonical `UploadFile<'upload>` (moved verbatim incl. `len`/`prefix`, now `pub`) + borrowed `UploadInput<'a>` (13 fields).
- `src/jobs/worker_ctx.rs` — `WorkerCtx { store, clock, mailer, blobs, streaming, federation, extra }` + `new` (Clone).
- Wiring only in shared files: `pub mod` lines in `src/application/mod.rs`, `src/jobs/mod.rs` (parent-merged per AGENTS.md).
- Tests (uniquely named): `tests/s0_page_cursor_check.rs` (3), `tests/s0_media_input_check.rs` (4), `tests/s0_search_admin_page_check.rs` (4), `tests/s0_worker_ctx_check.rs` (2).

Delegation: 4 subagents, disjoint new-file sets, shared `mod.rs` owned by parent; subagents left changes uncommitted. One deviation: `AscendingPage` uses named fields (spec's tuple-struct syntax is invalid Rust).

## Verification (native Windows toolchain, no WSL)
- Toolchain: `stable-x86_64-pc-windows-gnu` + WinLibs `gcc 16.2.0`/`dlltool 2.47` on PATH (pre-existing env lacked them on PATH; `cargo check` on `aws-lc-sys`/`ring`/`getrandom` failed without it).
- `cargo check --all-targets`: exit 0. Warnings all pre-existing (unused imports in `tests/fix_*.rs`, `adv2_*`; `pid` unused in `media/transcode.rs`). Zero warnings from new S0 files.
- `cargo test --lib`: 242 passed, 0 failed.
- `cargo test --test s0_*` (4 targets): 13 passed total (3 + 4 + 4 + 2), 0 failed.
- `rustfmt --edition 2024 --check` on the 5 new files: clean. Whole-tree `cargo fmt` NOT run (tree is not fmt-clean under rustfmt 1.99; reformatting would be unrelated churn).
- `git status` at commit: only the 5 + 2 + 4 files above.

## Open questions / next
- S1 (ports split) is unblocked and next; S0 types get adopted slice-by-slice (S4–S11), not here.
- `UploadFile` still duplicated in `application/media.rs` until S4 removes it (intentional).
- `s0_worker_ctx_check.rs` uses a `#[path]` shim (mod wiring landed after); S8 may replace with `birchtree::jobs::worker_ctx` import.
