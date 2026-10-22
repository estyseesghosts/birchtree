# Adv2 verification (Wave 1 gate, 2026-10-22)

Base: `8712ae8` (groundwork) + Wave 1 workstreams A–E (uncommitted at gate).
Target dir: `C:\Users\julie\AppData\Local\Temp\opencode\birchtree-groundwork-target`
Toolchain: native Windows `cargo`, no WSL.

## `cargo check --all-targets`

- Green. Only pre-existing warnings (`transcode.rs` unused `pid`,
  `fix_emoji_cache` unused imports).
- Log: `C:\Users\julie\AppData\Local\Temp\opencode\wave1-check.log`
  (tail: `Finished dev profile`, no `^error` lines).

## `cargo test` (full)

- Exit 0. 102 suites `test result: ok`, 0 failed.
- Doctests: `0 passed, 0 failed, 3 ignored` — the baseline `E0425 Job`
  contamination (groundwork mid-run edit) is fixed.
- Baseline comparison: no new failures vs Phase 0
  (`docs/logs/2026-10-22-adv2-baseline.md`; baseline had all suites pass
  plus the one contamination doctest error, now gone).
- Log: `C:\Users\julie\AppData\Local\Temp\opencode\wave1-test.log`

## New red-to-green suites (45 tests)

- `adv2_a_emoji` 6/6 (F2 admission, disabled, rollback, honest path)
- `adv2_a_fetch` 9/9 (F1 evict-only-on-budget, N2/N3 caps, N5 heal, lying CL)
- `adv2_a_settle` 7/7 (F6 fault injection, top-up, rollback)
- `adv2_b_cache` 6/6 (B1 dedupe refcount=2, double-evict, reopen, legacy decode; B3 7d retention)
- `adv2_b_recovery` 4/4 (B2 crash-window, TTL, delete-error, lost-row adopt)
- `adv2_c_ordering` 8/8 (C1 atomic seq, C2 index vs oracle, C3 park cap)
- `adv2_d_remote_media` 5/5 (N1 remote_url for pending/failed/video, local null, visibility parity)

Representative green lines are in the wave1-test log
(`adv2_a_emoji` through `adv2_d_remote_media` blocks).

## Shared-snippet fixes applied by parent at gate

- `tests/storage_cache_evict.rs`: `full_cap_bytes` 256K → 1M (A snippet 1;
  N3 class caps raised admission floor above the test's 16K/20K fixtures).
- `tests/fix_emoji_cache.rs`, `tests/gaps_federation_tail.rs`: 10-arg
  `store_remote_emoji` (store, blobs, cache, ledger, …) (A snippet 2).
- `tests/common/mod.rs`: reverted stray rustfmt both times, not committed.

## Scope check

- `git diff --name-only` at gate shows only Wave 1-owned files plus the
  three shared test files above. No `ports`/`schema`/`config`/`domain`
  edits beyond groundwork. `FEDERATION.md` committed separately (`8c54bc4`)
  per operator request, not part of this wave.
