Status: Draft

# birchtree storage overhaul — agent implementation plan

Parent reads first: `birchtree.md` §§2–5,7, `docs/remote-media-cache-plan.md` §§2–6, `docs/TODOs.md` (remote-media + verification debt), `AGENTS.md` delegation + config + state rules, latest `state/logs/`. `git status --short` clean before start. Subagents leave changes uncommitted; parent verifies then commits everything at the end (explicit operator instruction for this batch).

## Decision (binding)

* One `birchtree.redb`. No split-metadata-DBs — breaks one-txn `state+JOBS+media-metadata` atomicity (`birchtree.md` §5), doubles backup/lock pain.
* Two byte pools, two `FsBlobStore` roots:
  * `media_durable/` = local uploads. Never evicted. No per-user quota in v1 (see deferred below).
  * `media_cache/` = remote fetches. Global + per-class caps, byte-LRU+TTL, evict-bytes-only, keep metadata + `source_url` for re-fetch.
* Hot small (avatar/header/emoji) stays a reserved class inside cache, not a third store: 128 MiB / 30d + URL revalidate. Matches `docs/remote-media-cache-plan.md` §4 budget (512 MiB global = 128 + 256 + 128 + 0 A/V stream-only).
* Per-row keys retained for v1 (`remote/{media_id}`, `federation/media.rs:83`). No content-addressed sharing, so no `BLOB_REFCOUNT` in v1 (pinetree difference — deliberate, see §"Deferred"). Safe eviction = delete the row's own key. Never touch durable local media.

## Deltas vs pinetree `docs/database.md` (why this is not a copy)

* No `BLOB_REFCOUNT` / `DURABLE_BY_OWNER` tables in v1, no `DRIVE_CAPACITY_EXCEEDED`. Mastodon has no Drive/quota upstream; a per-user cap would be a new `D` divergence + operator decision (`docs/TODOs.md` media-caps unsettled). Grandfather/over-quota logic deferred with it.
* Error shape is Mastodon (`422 {"error":...}`, `404`, `503`), never Misskey Drive envelopes.
* `FsBlobStore` today is direct `put(bytes)`/`get`/`delete` (`src/media/blob_store.rs:50-87`); pinetree assumes tmp+rename + `.len` sidecar. Agent C adds streaming + tmp-rename here.
* Current fetch buffers fully in `Vec` (`src/federation/media.rs:65-74`) and `upload_media(bytes: &[u8])`; byte-cap abort exists but no stream-to-disk. Agent C fixes both paths.
* Reconciler scope is smaller: orphans, missing blobs, stale metadata, unfinished deletions. No `.len`/MD5 rows (no sidecars yet).
* Observability routes via log + `/health` interim; admin metrics deferred (no admin endpoint before its phase — `remote-media-cache-plan.md` §5.6).
* Open gap carried, not absorbed: `federation/media.rs:46-52` sends unsigned `GET Accept: */*` while `federation-contract.md` requires instance-actor-signed dereferences — record keep-unsigned vs sign decision in `docs/federation-contract.md`.
* No FDB/S3. All pool access via `BlobStore` trait (`src/application/ports.rs:326-335`); no new local-FS-only assumptions.

## Parent-owned shared files (subagents propose snippets, never edit)

* `docs/remote-media-cache-plan.md`, `docs/endpoint-manifest.md`, `docs/TODOs.md`, `docs/testing.md`, `docs/compatibility-profile.md` (new `D` row only if quota ever lands)
* `src/application/ports.rs` (`BlobStore`, `MediaStore`, `CacheStore`, `JobKind`)
* `src/persistence/redb/mod.rs` + `schema.rs` (new tables/indexes) + `migrations.rs`
* `src/config.rs` + `src/main.rs` wiring (+ `config.example.json`)
* `src/federation/media.rs` ingest trigger + `src/api/mastodon/endpoints/media.rs` error surfacing
* `tests/persistence_conformance.rs`

Parent Step 0 lands skeletons only:

1. Config keys (all optional, defaults, `validate()`, round-trip test per `AGENTS.md`, `deny_unknown_fields` stays):
```json
{
  "durable_dir": "./data/media_durable",
  "cache_dir": "./data/media_cache",
  "remote_cache": {"mode": "on_demand", "global_cap_bytes": 536870912, "avatar_emoji_cap_bytes": 134217728, "preview_cap_bytes": 268435456, "full_cap_bytes": 134217728, "avatar_retention_days": 30, "warming": false}
}
```
Defaults: `mode=on_demand`, 512 MiB global, warming off. `validate()`: caps > 0, `cache_dir != durable_dir`, global >= sum reserved, `MAX_REMOTE_MEDIA_BYTES` (40 MiB) stays per-file ceiling inside class/global budgets. `mode = disabled` is rejected at startup (adversarial-2 F1) until a non-persisting delivery path exists.
2. New redb tables (names only, no logic): `REMOTE_CACHE{source_url,variant -> blob_key,class,size,last_use,expiry,etag,refcount}` + `CACHE_ACCOUNTING{singleton: total, per-class, in-flight}`. `REMOTE_CACHE` key = `(source_url, variant)`; value holds per-row store-key — index ≠ bytes. Dedupe with refcounts (adversarial-2 B0, operator choice): a second blob for the same pair bumps `refcount` and shares the first bytes. Schema v24 adds `DELIVERY_ACTIVE{(host, seq, job_id) -> job id}` (adversarial-2 C2) plus the cache `refcount` backfill.
3. `JobKind`: `CacheEvict`, `ReconcileStorage`. Payloads < 64 KiB, bounded batches 500, no blobs in txn. `FetchRemoteMedia` is reused (trigger conversion), not duplicated.

## Agent partition (disjoint writes, no collisions)

Subagents share one worktree — stay inside owned files. Shared files above are parent-merge only. Each agent gets a uniquely-named test file, leaves changes uncommitted, never runs `git commit/stash/reset/checkout --`.

* **A — two stores + migration.** Owns: `src/media/blob_store.rs` (two-instance constructor, tmp+rename write), `src/media/mod.rs`, `tests/storage_two_pools.rs` (new). Proposes snippets for: `main.rs` (open `durable` + `cache` roots), `ports.rs` (`BlobStore` handle enum `Durable|Cache`). Tasks: keep current `media/` path as durable root, new `media_cache/` root; lazy move — remote rows with bytes in durable stay readable, reconciler copies + repoints; no flag day. Tests: durable write never lands in cache root and vice versa, pre-migration blob still `get()`s.
* **B — cache accounting + eviction + on-demand flip.** Owns: `src/media/remote_cache.rs` (no longer stub), `src/federation/media.rs` (metadata-first + fetch-on-request + in-flight reservation), `tests/storage_cache_evict.rs` (new). Proposes snippets for: `ports.rs` (`CacheStore`), `redb` cache fns, ingest trigger (`federation_inbound.rs` FetchRemoteMedia enqueue → metadata-first). Tasks: ingest records `REMOTE_CACHE` metadata only; `GET /files|/proxy` triggers fetch-on-first-request with in-flight reservation (concurrent fetches cannot overshoot cap); byte-LRU + TTL per class, reserved avatar/emoji never borrowed by images; evict = remove mapping → update accounting → `delete` own key; warming stays `false`, relay-excluded. Tests: concurrent-download cap-never-exceeded, evicted re-fetches while source lives, avatar reserve survives image pressure, local uploads survive aggressive eviction.
* **C — stream-to-disk + reconciler + observability.** Owns: `src/media/fetch_stream.rs` (new), `src/media/reconciler.rs` (new), `tests/storage_reconcile.rs` (new). Proposes snippets for: `ports.rs` (`put_streamed`), `observability.rs` counters. Tasks: `fetch_and_attach` + `upload_media` stream to `tmp/{rand}.part` under byte cap (dishonest-length/chunked covered, mid-body abort); reconciler idempotent sweep: orphans, missing blobs, stale metadata, unfinished deletions; counters without shell: `bytes_used, per_class, hits/misses, fetched, evictions, failed_origins`. Tests: oversize aborted mid-stream HTTP+HTTPS, torn write reads `None` then heals, reconciler converges to zero orphans.

FDB parity explicitly out. No `use foundationdb`, no S3 — but no new code that assumes local-FS-only paths; all pool access via traits.

## Verification gate (parent runs)

```bash
cargo check --all-targets
cargo test --test storage_two_pools --test storage_cache_evict --test storage_reconcile
cargo test
bash scripts/smoke-all.sh
```

Plus: fill cache to 110% with concurrent jobs → stays ≤ cap; `rm` a blob file → reconciler re-fetches or marks missing; aggressive eviction → local uploads untouched. Paste real output into `state/logs/YYYY-MM-DD-storage-overhaul.md`, update `TODOs.md` + cache plan in place, never rewrite old logs. Then `git status --short` review + commit everything (this batch only — explicit instruction).

## Prod-2 deltas (2026-10-22, schema v23)

- `BlobStore::cache_root_path` (port addition): the public file route
  streams `remote/…` keys from the cache pool after the durable root — no
  full-object buffering; buffered `get()` remains only for rootless test
  doubles. Evidence: `tests/fix_media_http_stream.rs`.
- Eviction is durable-first everywhere (`evict_pressure_durable_first`,
  shared by fetch-time admission and `CacheEvict`): victims unmapped
  (rows + counters, one transaction) before any blob delete; durable
  failure deletes nothing and the fetch does not proceed. Startup enforces
  caps after restore, before listen. Evidence:
  `tests/storage_cache_evict.rs`.
- Fetch commits the durable byte row (`cache_mark_stored`) before the
  ledger settlement, so admitted bytes are visible to every eviction pass;
  attachments classify after the sniff (`class_for_fetch`) and audio/video
  (`Stream`) is never persisted. Evidence: `tests/fix_media_class.rs`.
- Remote emoji rows (`remote/emojis/`, variants `emoji`/`emoji_static`)
  are durable byte rows under the reserved class: evictable, refetchable.
- `DELIVERY_SEQ` table (`host -> next sequence`, schema v23): every
  delivery enqueue mints its destination's monotonic `seq`; the send path
  parks N+1 while N is unfinished. Evidence:
  `tests/fix_delivery_order.rs`.
- `prune_quarantined` (age-ordered, bounded, delete-only) rides the
  maintenance tick; enqueue quarantines oversize payloads on insert.
  Evidence: `tests/fix_quarantine_prune.rs`.
