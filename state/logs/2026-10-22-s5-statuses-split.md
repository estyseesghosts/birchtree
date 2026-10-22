# 2026-10-22 — S5 statuses split (decomposition proposal)

## What changed
Application layer (`application/statuses.rs`, 1156 lines → dir, shim
deleted):
- `statuses/mod.rs` (re-exports preserving all pre-existing
  `crate::application::statuses::*` paths), `create.rs` (`PollCommand`,
  `CreateStatusCommand`, `CreateStatusResult`, `create_status` — 9-param
  signature unchanged), `create_validate.rs` (limit consts, all `ERR_*`
  byte-identical, `text_too_long_error`, `count_length`,
  `build_poll_spec`), `create_mentions.rs` (tags/mentions/render/regex
  helpers), `update.rs` (`UpdateStatusCommand`, `update_status`,
  `show_status`, `status_source`, `delete_status`, `statuses_by_ids`,
  `account_statuses`), `threads.rs` (`ContextTree`, `status_context`,
  `status_history`). 1224 lines total incl. new module rustdoc + split
  `use` blocks.
- `<80-line orchestrator target NOT met`: `create_status`/`update_status`
  moved verbatim; stage extraction is follow-up material. No behavior
  change taken here.

Persistence layer (`redb/statuses.rs`, 1913 lines → 7 files, shim deleted):
- `statuses_read.rs` (500 lines: 6 scan-cap consts single home,
  `read/write_status_txn`, `ensure_id_unused`, `maintain_timeline_index`,
  `status_at`, inherent `impl RedbStore { can_write,
  highest_persisted_id }`, 8 read `*_impl` fns),
  `statuses_create.rs` (`PreparedCreate`, `idempotency_key`, stats,
  `enqueue_fan_out`, `insert_row_txn`/`write_indexes_txn`/
  `enqueue_create_jobs_txn`, `create_status_impl`),
  `statuses_update.rs` (643: `ScalarDiff`/`PollPlan`,
  `norm_spoiler_text`/`same_shape_poll` named fns, `diff_media`/
  `diff_scalars`/`plan_poll`/`apply_patch_txn`/`enqueue_edit_jobs_txn`,
  `update_status_impl`, `rewrite_status_tags_impl`),
  `statuses_delete.rs` (quote fns, `clear_poll_votes`,
  `newest_live_status_at`, cascades, `delete_status_impl`),
  `statuses_interactions.rs` (715: `PinRecord` with `pub(super)` field,
  `reblog_links`, 13 interaction `*_impl` fns),
  `statuses_impl.rs` (new, parent-written: the SINGLE
  `impl StatusStore for RedbStore` — 25 one-line delegations; E0119
  forbids splitting a trait impl across files).
- No transaction-boundary changes: each `*_impl` opens exactly one
  `begin_write`/`begin_read` as before. `DeletedStatus` import fixed to
  `application::ports`; `Bookmark`/`Favourite` to
  `domain::{bookmark,favourite}`; self-import of `read/write_stats_txn`
  removed (defined locally in `statuses_create`).
- 13 external call sites repointed `super::statuses::` →
  `super::statuses_read::` (`federation_content.rs` 5, `quotes.rs` 5,
  `media.rs`/`notifications.rs`/`polls.rs` 1 each).

## Verification
- `cargo check --all-targets`: exit 0, zero new warnings. All 14 warning
  locations pre-existing: `admin_infra_impl.rs` braces (S6),
  `transcode.rs:742 pid`, `fix_*.rs`/`adv2_*` test imports (baseline set).
- `cargo test` (full): 1153 passed, 0 failed — matches pre-slice baseline.
- `rustfmt --edition 2024 --check`: clean on all 11 new leaf files +
  parent-written `statuses_impl.rs` and `statuses/mod.rs`. Pre-existing
  tree-wide fmt diffs (rustfmt 1.99) left untouched; no renamed line
  introduces a new diff.
- `scripts/smoke-all.sh` NOT run (needs bash; `C:\Windows\System32\bash.exe`
  is a WSL shim — never use WSL). Native `cargo check` + `cargo test`
  gates used instead.
