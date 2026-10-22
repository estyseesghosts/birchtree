# 2026-10-22 — adversarial-2 workstream A (admission + fetch path)

## Scope (owned files only)

- `src/media/remote_cache.rs` — N3 class caps, `shortfall_for` fix, RAII
  `Reservation` guard (+ unit tests).
- `src/federation/media.rs` — A1 admission, A2 emoji admission, A3 settle
  (`SettleStore` seam + `settle_staged`), A4 heal classification, N2 stream
  cap.
- `src/media/fetch_stream.rs` — owned, no change needed (caps are applied
  by the caller).
- `src/jobs/worker.rs` — FetchRemoteEmoji arm only (two added args).
- New tests: `tests/adv2_a_fetch.rs` (9), `tests/adv2_a_emoji.rs` (6),
  `tests/adv2_a_settle.rs` (7).

A5 (staging_root switch) explicitly NOT done — fetch still uses
`root_path()`; Wave 2 moves it.

## What changed

- `reserve` enforces Full/Preview class caps (stored+inflight+size > cap →
  `BudgetExhausted`); `shortfall_for` returns max(global, class) overage so
  eviction never understates.
- Fetch admission evicts only on `BudgetExhausted`; other refusals
  (Unsupported incl. Disabled, Oversize) map via `ledger_outcome` with no
  eviction. Disabled mode also gates the cached-blob fast path.
- `settle_staged` (new, `pub`): N2 top-up-or-fail, durable row, ledger
  commit with durable-row eviction on refusal, media-row staging with full
  rollback; stream cap = live reservation.
- `store_remote_emoji` now takes `cache: &dyn CacheStore` and
  `ledger: &Mutex<CacheLedger>`; reserves `2*size` AvatarEmoji, stages both
  blobs, writes durable rows, commits each copy, rolls everything back.
- Heal path classifies via `class_for_fetch`; Stream bytes are deleted,
  never counted or staged.

## Verification (all with
`$env:CARGO_TARGET_DIR=C:\Users\julie\AppData\Local\Temp\opencode\birchtree-ws-a-target`)

- Red phase (pre-fix): `disabled_mode_evicts_nothing_and_refuses` failed
  ("victim blob survives" — old code evicted on Unsupported),
  `cached_video_blob_is_neither_counted_nor_staged` got `Ok(())`,
  `lying_content_length…` got `Ok(())` with 1 MiB staged on a 16-byte
  holding.
- Green phase: `cargo test --test adv2_a_fetch` 9/9,
  `--test adv2_a_emoji` 6/6, `--test adv2_a_settle` 7/7,
  `cargo test --lib media::remote_cache` 10/10, full `cargo test --lib`
  242/242, plus `fix_media_class` 2/2, `a_signed_media_fetch` 6/6,
  `fix_media_pools` 6/6, `fix_media_http_stream` 3/3,
  `storage_reconcile` 6/6, `storage_two_pools` 3/3, `crash_recovery` 3/3,
  `persistence_conformance` 18/18.
- `cargo check --all-targets`: only hard errors are the two known
  snippet sites below. Pre-existing `adv2_c_ordering` breakage is gone
  (owner fixed it concurrently).

## Parent merge needed (shared files, not touched)

1. `tests/storage_cache_evict.rs` — `test_cfg`: `full_cap_bytes:
   256 * 1024` → `1024 * 1024`. Fixes all 4 failures
   (`evicted_entry_refetches…`, `avatar_reserve…`, `refusal_shapes…`,
   `local_uploads…`). Validated by replicating the four scenarios with the
   fixed cfg in a throwaway file (4/4 green, file deleted).
2. `tests/fix_emoji_cache.rs` (2 sites) and
   `tests/gaps_federation_tail.rs` (4 sites): pass `&*store` as the new
   `cache` arg plus a ledger, e.g.
   `let ledger = Mutex::new(CacheLedger::new(RemoteCacheConfig::default()));`
   then `store_remote_emoji(&*store, &pools, &*store, &ledger, …)`.
   Semantics unchanged (all refusal sites reject before admission).
3. No `config.rs` change needed: Disabled rejection + doc comment already
   landed in groundwork and are accurate.

## Open questions / deviations

- Fault seam is narrow traits (`SettleStore`, `CacheStore`, `BlobStore`
  doubles) rather than a full-`Store` wrapper: `Store` is a blanket impl
  over ~28 subtraits (~300 methods); the narrow seam injects at the same
  three fault points (`cache_mark_stored`, `stage_remote_media`, blob
  `delete`).
- N2 fetch-level: hyper enforces `Content-Length` framing (reads exactly
  the declared bytes; reports `None` when chunked TE is present), so a
  lying declaration can never over-deliver through this client — the
  reserved-size cap is defense-in-depth plus the settle backstop (covered
  by `top_up_*` tests). The TE+CL framing case is pinned as a guard test.
- Blob-`delete` fault tests assert inflight==0 and no row claiming bytes;
  the surviving blob is reconciler residue by the documented crash-window
  contract.
- `Config::validate` still rejects Disabled at startup (groundwork); the
  fetch-path Disabled handling is belt-and-braces for tests/embedding.
