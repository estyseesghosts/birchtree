# 2026-10-22 — S4 media upload pipeline (decomposition proposal)

## What changed
- `src/application/media_input.rs` finalized (S0 type): canonical
  `UploadFile` + borrowed `UploadInput` (13 fields).
- New `src/application/media_validate.rs`: 8 pure validators (steps
  1–6b) with byte-identical `ERR_*` strings; `parse_focus_or_err`
  delegates to `media::parse_focus`; `check_thumbnail` returns the
  `(bytes, sniffed)` pair for staging.
- New `src/application/media_commit.rs`: `StagedUpload` guard threading
  (`stage_bytes` → `commit_row` → `finish_inline`), verbatim bodies;
  `new_shard`/`shard_key` live here now (sole users).
- `src/application/media.rs` (1031 → ~700 lines): `upload_media_with_input`
  orchestrator (<50 lines) sequencing validate → stage → commit → finish;
  the three 13/14-param wrappers DELETED (S0 allowed removal in S4);
  `UploadFile` enum deleted (uses `media_input::`); `new_shard`/`shard_key`
  deleted (moved); `parse_focus`, `human_size`, `file_too_large_error`,
  `sniff_accepted`, `sibling_key` stay (used by update paths / endpoints /
  tests); all three `too_many_arguments` allows gone (one remains on
  `commit_row`, justified: 10 params, single-transaction builder).
- Callers migrated to `UploadInput`: `endpoints/media.rs` (2 sites),
  7 in-file test calls + 2 integration tests
  (`phase7_gaps_round2_a`, `coreext_alttext`).
- Deviations logged, no behavior change: test-only `redb` import KEPT
  (no `Store` test double exists anywhere; building one is S12 sweep
  material — test-only `#[cfg(test)]` import, zero production impact).

## Verification (native Windows, shared target)
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test` (full, with S6/S7a/S8/S11): 1153 passed, 0 failed —
  media tests green unchanged (proves strings preserved).
- `rustfmt --edition 2024 --check` on touched files: clean (file-scoped;
  pre-existing tree-wide drift untouched).
