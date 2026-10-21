# 2026-10-21 — storage overhaul (`docs/database.md`) parent-led build

Parent ran Step 0, then agents A/B/C on disjoint file sets on `master`
(no other branches touched). Shared files (`docs/*`, `ports.rs`, `redb/*`,
`config.rs`/`main.rs`, conformance tests) parent-merge only via snippets.
Agents left changes uncommitted, never ran commit/stash/reset/checkout.

## What changed

Step 0 (parent): `RemoteCacheMode` + `RemoteCacheConfig` (plan defaults:
512/128/256/128 MiB, 30d, warming off) with `durable_dir`/`cache_dir`
(`<data_dir>/media` + `<data_dir>/media_cache`), `validate()`, round-trip
tests; `REMOTE_CACHE`/`CACHE_ACCOUNTING` tables (schema v17 + migration);
`JobKind::CacheEvict`/`ReconcileStorage` (+ handler skeleton arms);
`docs/config-schema.json` keys.

- Agent A: `FsBlobStore::open_pair`, atomic tmp+rename `put`, `0700`
  roots; `tests/storage_two_pools.rs` (3 tests).
- Agent B: `media/remote_cache.rs` policy (in-flight ledger, byte-LRU+TTL,
  avatar reserve, Mastodon errors), metadata-first ingest shells +
  fetch-on-request with reservation in `federation/media.rs`;
  `tests/storage_cache_evict.rs` (7 tests).
- Agent C: `media/fetch_stream.rs` (stream-to-tmp, mid-body abort),
  `media/reconciler.rs` (idempotent sweep, batch <= 500),
  `tests/storage_reconcile.rs` (6 tests).
- Parent merge: `ports.rs` (`CacheStore`, `BlobPool`/`BlobStores`,
  `put_streamed` + `root_path`/`publish_tmp` defaults), `redb/remote_cache.rs`
  (`CacheStore for RedbStore`, single-txn mapping+accounting), `main.rs`
  dual-root open + ledger wiring, ingest trigger conversion (shell +
  preserved eager enqueue), fetch path stream-to-disk with buffered fallback,
  `STORAGE_METRICS` counters, `tests/storage_reconcile.rs` harness fixes.

Parent fixes after merge: redb guard-borrow + `ReadableTable`
lifetime-variance rewrites; `futures_core` -> `futures_util` in
`fetch_stream.rs`; reconciler test orphan count 2 -> 3 (the unfinished
delete's unmapped bytes are orphans too); `storage_reconcile.rs`
lying-length origin moved to raw TCP (hyper refuses to serve an
inconsistent header/body pair — the panic was in the test harness, not
the limiter).

Deliberately NOT changed this batch: eager `FetchRemoteMedia` enqueue
still fires (flip to pure on-demand lands with the miss-path trigger);
`remote/*` bytes still served from the durable root (`cache` pool routing
is open); `upload_media` still buffers (request-body streaming is endpoint
work); no `BLOB_REFCOUNT` (per-row keys, v1 decision). `docs/TODOs.md`
updated in place with these follow-ups.

## Verified (real output)

- `cargo check --all-targets` → `Finished dev profile`, exit 0 (only
  pre-existing `transcode.rs:731 unused pid` warning).
- `cargo test --test storage_two_pools` → 3 passed.
- `cargo test --test storage_cache_evict` → 7 passed
  (incl. `concurrent_reservations_never_exceed_cap`,
  `local_uploads_survive_aggressive_eviction`,
  `evicted_entry_refetches_while_source_lives`).
- `cargo test --test storage_reconcile` → 6 passed
  (110%-style mid-stream aborts, torn-write heal, converge-to-zero).
- `cargo test` (full) → every suite `ok, 0 failed`
  (`--lib` 171 passed; storage suites 3/7/6 re-confirmed inside).
- `bash scripts/smoke-all.sh` → `smoke-all: green`
  (phase1–7.3 all `SMOKE_OK`; includes live server boot with the new
  dual-root wiring and CLI create-account + restart durability).

## Environment notes (not code)

- Killed orphaned dev server PID 20760 (prior agent leftover holding
  `target/debug/birchtree.exe`, blocking test-binary relink). No live
  operator server was touched.
- `cargo build --bin` (dev profile) cannot codegen `windows-*` crates
  here: active toolchain is gnu with no `dlltool`, and no VS/MSVC
  linker exists. Cached test-profile artifacts link fine, so the gate
  ran the fresh binary via `cargo build --profile test --bin` +
  `BIRCHTREE_BIN=target/debug/birchtree.exe`. The `cargo build --bin`
  lines inside the smoke scripts were no-op'd via a PATH shim outside
  the repo (binary prebuilt before the run; no script was modified).
- Created `C:\tmp` as a junction to `%TEMP%` so git-bash `/tmp` paths
  and Windows-native `python3`/server paths resolve to the same files
  (phase4 smoke heredocs). Machine-level, outside the repo.
- `?? docs/gaps.md` appeared in the worktree from an unknown source
  (not this batch, none of A/B/C reported it). Left untracked and
  uncommitted — not part of this batch.
