# Adv2 baseline (2026-10-22, base `e82343e`)

## Commands

- `cargo check --all-targets` (target dir outside repo): pass, only pre-existing
  unused-import warnings.
- `cargo test` (same target dir): all integration/unit suites pass, 0 failed.
  Full per-suite `test result: ok` lines are in the raw log
  (`C:\Users\julie\AppData\Local\Temp\opencode\baseline-test.log`).

## Pre-existing vs contamination

- All `tests/*.rs` suites: pass (0 failed). Representative tail:
  `storage_cache_evict`, `storage_reconcile`, `fix_media_class`,
  `fix_worker_sched`, `fix_delivery_order`, `persistence_conformance`,
  `z_schema_v23_probe`, `z_worker_jobs_probe` all `ok`.
- `Doc-tests birchtree`: 1 error `E0425 cannot find type Job` at
  `src/application/federation_ports.rs:118` (`enqueue_delivery` signature).
  This is NOT pre-existing: the baseline `cargo test` started on the clean
  tree, but groundwork edits landed while the doctest phase ran, so rustdoc
  saw the half-finished groundwork (missing `Job` import). Fixed in the
  groundwork commit by importing `Job`. No other failures observed.
- `git status --short` at baseline start was clean except for untracked
  helper docs (`docs/adversarial-*.md`, `rust_style_guide.html`,
  `scratch_vendor_lock.bak`) which are not part of the build.

## Pinned behaviour read before changes

- `tests/fix_media_class.rs` (audio quarantine, image persist).
- Cache/reconcile suites, FIFO/ordering tests
  (`tests/fix_worker_sched.rs`), and
  `rfc9421_shaped_request_is_refused_without_state_change`.
- Grepped callers of every signature changed in groundwork
  (`cache_mark_stored`, `root_path`, `REMOTE_CACHE`, `DELIVERY_SEQ`).
