Status: Draft

# Birchtree — remote-media cache plan (from `storage-cache.md` review)

Source: `/root/storage-cache.md` (upstream comparison: Akkoma / Mastodon /
Misskey + shared-policy recommendation). This doc is the Birchtree-specific
forward plan. Pinetree's counterpart is
`pinetree/docs/remote-media-cache-plan.md` — same policy concepts, different
schema (Birchtree: Mastodon-compatible attachments; Pinetree:
Misskey-compatible media semantics).

## 1. Where Birchtree stands

Three distinct pieces, not one "stub":

- (a) **Cache-lifecycle stubs.** `src/media/remote_cache.rs:1` ("Remote media
  cache (stub; Phase 6+)") and `src/federation/remote_media.rs:1`
  ("Remote media (stub; Phase 6+)") — no budget, eviction, or lifecycle yet.
- (b) **Working eager fetch path.** Ingest creates media rows + records
  sources (`src/application/federation_inbound.rs:129-137`,
  `store.set_remote_media_source`), enqueues one `FetchRemoteMedia` job per
  attachment (`federation_inbound.rs:178-183`; `JobKind::FetchRemoteMedia` in
  `src/domain/job.rs:53`, wired in `src/jobs/worker.rs:71`). The worker
  (`src/federation/media.rs:24-102`) applies URL policy, `Content-Length`
  early abort (`:61`), streaming byte-count abort (`:65-74`),
  `ingest::sniff` + `validate` (`:75`), stages bytes via `blobs.put` (`:83-87`)
  and `stage_remote_media` (`:90`), backed by the `REMOTE_MEDIA_SOURCE`
  table (`src/persistence/redb/federation_content.rs:381-441`).
- (c) **Shared store placement.** Remote bytes land in the same `FsBlobStore`
  as local uploads (`src/media/blob_store.rs:12-30`, `0700` root, traversal
  rejection `:33-46`) under per-row `remote/{media_id}` keys
  (`federation/media.rs:83`) — no sharing, but also no isolation.

The task is therefore a **conversion + split** (eager → lazy, shared store →
separate stores), not greenfield construction. Current phase is Phase 5
(streaming); federation is Phase 6 (`birchtree.md` §7). Freeze this design
before Phase 6 media work.

> Verification 2026-10-20 (docs worker W3, read-only vs HEAD): the stub
> premise above still holds. `src/media/remote_cache.rs:1` and
> `src/federation/remote_media.rs:1` are both still one-line stubs; the eager
> fetch path is as described (`FetchRemoteMedia` enqueue at
> `src/application/federation_inbound.rs:181`,
> `set_remote_media_source` at `:134`, worker at
> `src/federation/media.rs:23-102`, per-row `remote/{media_id}` key at `:83`);
> line citations elsewhere in this plan have drifted by a few lines as code
> moved (e.g. `REMOTE_MEDIA_SOURCE` table/fns now at
> `src/persistence/redb/schema.rs:375` /
> `src/persistence/redb/federation_content.rs:455-479`) but the substance is
> unchanged. No build started; plan stays a plan.

## 2. Corrections adopted from the review

1. **Byte budgets + eviction, not TTL alone.** Time-based expiry and a global
   byte cap solve different problems; we need both. Note the review's
   correction: Akkoma's onion-federation example does set `max_size=10g` —
   the failure mode is configs with a long inactivity period and no size cap.
2. **Per-class limits AND a hard global cap.** Otherwise popular avatars eat
   the budget meant for post images. Avatar/emoji entries need revalidation
   (source URL may serve changed content).
3. **Lazy fetch by default — a behavior change, not a new build.** Replace
   the ingest-time `FetchRemoteMedia` enqueue (§1b) with metadata-first
   records + fetch on first media request. Preserve the
   `stage_remote_media` → `ProcessMedia` chain
   (`federation_content.rs:397-429`). Warming (if any) reuses the same job
   kind behind config, rate limit, budget, and relay exclusion.
4. **Content addressing ≠ identity.** SHA-256 dedups byte-identical files,
   not resized/re-encoded variants. (Scoped note: the review's SHA-256 +
   shared-blob fix, `storage-cache.md:54-58`, applies to Pinetree, which
   already content-addresses. Birchtree uses per-row `remote/{media_id}`
   keys with no sharing — see the §3 dedup decision.)
5. **Durable uploads isolated.** Local users' originals must never disappear
   under remote-cache pressure — separate storage pools (simplest safe
   option). Concretely: migrate existing `remote/*` keys out of the shared
   `FsBlobStore` into the new remote-cache store; enforce "no disposable
   files in the local-upload namespace".
6. **Bounded ingest + reconciliation.** `Content-Length` is advisory only:
   the existing streaming abort (`media.rs:61-74`) already counts bytes and
   quarantines oversize bodies — keep that shape under the new trigger.
   Filesystem + redb work cannot commit atomically, so a periodic idempotent
   reconciler is required. (Terminology note: Akkama's `prune_objects` is
   about DB objects, not media-cache eviction — do not cite it as a
   cache-eviction precedent.)
7. **Budget arithmetic must add up.** The review's illustrative 5 GB default
   had sub-budgets totalling 3.5 GB. Our starting budget below sums exactly
   (512 MiB global).

## 3. Design to freeze before Phase 6

- **Keep `FsBlobStore` for durable local media.**
- **New separate remote-cache store** + migration of existing `remote/*`
  keys out of the shared store.
- **New remote-cache metadata model** (redb): source URL, blob key, media
  class, size, fetch time, last access, expiry, HTTP validators (e.g. ETag).
  Metadata survives byte eviction.
- **Dedup decision (frozen for v1): per-row keys retained, no
  content-addressed sharing.** Rationale: current keys (`remote/{media_id}`)
  cannot collide, so there is no shared-blob hazard to fix here; adopting
  SHA-256 dedup would import Pinetree's reference-counting problem for no
  measured gain. Safe eviction therefore means "delete the row's own key".
  If dedup is ever adopted, it requires a reference index first, and §7's
  shared-blob test activates then (currently N/A — marked as such).
- **Ingest path:** federation records attachment metadata first (as today);
  bytes are fetched on first request unless the admin enables scoped
  warming (trigger conversion, §2.3).
- **Reuse existing architecture:** background-job system + storage-port
  traits; all filesystem work happens outside redb transactions
  (`birchtree.md` §5 + `AGENTS.md` architecture rules: one redb txn commits
  state + indexes + bounded durable jobs; network/media work after commit).
- **Mastodon-compatible attachment behavior** on the API surface
  (not Misskey Drive semantics).

## 4. Shared policy (same concepts as Pinetree)

Modes: `disabled` (no persistence; direct/streaming proxy delivery — no
proxy design yet) / `on_demand` (fetch on first request, cache, auto-evict)
/
`warm` (proactive fetch via separate queue + rate limit + budget).
**Default: `on_demand`, warming off.**

Starting budget — proposed starting values, not upstream defaults; tune from
measured workloads. Class caps sum to the global cap; unused class capacity
may be borrowed within the global cap:

| Media class | Max | Example retention |
|---|---|---|
| Avatars, headers, custom emoji | 128 MiB | 30 days |
| Image previews / thumbnails | 256 MiB | 7 days |
| Full image originals | 128 MiB | 3 days |
| Audio/video originals | 0 MiB persistent | stream / fetch on demand |
| **Global cap** | **512 MiB** | — |

Scope note: the only implemented remote-media path today is status
attachments (`federation_inbound.rs:129-137`, `federation/media.rs`). No
avatar/header fetch pipeline exists yet — the avatar/emoji row above
reserves budget for that future path; v1 enforcement covers attachments.

- Eviction: byte-weighted LRU + expiry checks (not LFU). Protected
  allocation for high-reuse avatars/emoji, never overriding the global cap.
- Admins may raise caps substantially (tens–hundreds of GB sponge profile),
  but the cache stays bounded.
- Relay-originated media: out of warming by default; if enabled, explicit
  limits under the same global cap. (No relay path exists yet —
  future-proofing.)

## 5. Implementation requirements (Birchtree adapters)

1. **Accounting:** total + per-class bytes, including in-flight fetches so
   concurrent downloads cannot overshoot without bound.
2. **Streaming safety:** keep the existing enforcement shape — SSRF shape
   policy, DNS-pinned resolver, `redirect(Policy::none)`, 5s connect / 10s
   total timeouts (`src/federation/net.rs:58-95,129-152`) plus
   `Content-Length` advisory check + streaming abort + `ingest::sniff` /
   `validate` (`src/federation/media.rs:61-81`). Reconcile the existing
   per-file `MAX_REMOTE_MEDIA_BYTES = 40 MiB` (`media.rs:20`, Mastodon
   default upload limit) with the new class caps: 40 MiB stays as the
   per-file ceiling inside the class/global budgets unless measured
   workloads justify a per-class split. Open gap (document, don't silently
   absorb): `federation/media.rs:46-52` sends an unsigned `GET` with only
   `Accept: */*`, while `federation-contract.md:36-40` requires
   instance-actor-signed dereferences with authorized-fetch handling —
   decide whether media fetch gets signing or records the exception.
3. **Safe eviction:** remove cache mapping → update accounting → delete the
   row's own blob key (per-row keys, no sharing per §3). Never touch durable
   local media.
4. **Recovery:** periodic idempotent reconciler (orphaned/missing blobs,
   stale metadata, unfinished deletions).
5. **Relay isolation:** as §4 (future-proofing; no relay path yet).
6. **Observability:** bytes used, utilization, per-class breakdown,
   hits/misses, bytes fetched, evictions, failed origin fetches. Admin API
   is absent (404) until Phase 7.3 (`birchtree.md` §7 Phase 7.3, §9), so
   either surface via an interim channel (log/health endpoint) or mark
   admin-visible metrics deferred to 7.3 — do not imply an admin endpoint
   exists before then.

## 6. Suggested build order

1. Freeze this design + metadata schema (this plan stays a plan; normative
   deltas go to `federation-contract.md` / `persistence-conformance.md`,
   which currently omit `REMOTE_MEDIA_SOURCE`, `stage_remote_media`, and
   the bench-run5 media notes in `federation-contract.md:139-176`).
2. Split the store: new remote-cache store + migrate `remote/*` keys out of
   the shared `FsBlobStore`; add metadata model + accounting, with unit
   tests for caps and eviction.
3. Convert the trigger: gate/replace ingest-time `FetchRemoteMedia` enqueue
   with metadata-first + on-demand fetch (default), preserving the
   `stage_remote_media` → `ProcessMedia` chain; warming reuses the same job
   kind behind config + relay exclusion.
4. Reconciler + observability (§5.6 routing decision first).
5. Optional scoped warming last.
6. Port the shared acceptance tests (§7) before claiming Phase 6 readiness
   for media.

## 7. Acceptance tests (must prove)

- Cache never exceeds configured cap under concurrent downloads (include an
  in-flight reservation test — the hard part, not just post-hoc totals).
- Local uploads survive aggressive eviction (requires the §3 store split;
  unprovable while `remote/*` shares the local store).
- Shared-blob premature deletion: N/A under the §3 per-row-key decision;
  activates only if content-addressed dedup is ever adopted.
- Oversized bodies are aborted mid-stream (dishonest/absent
  `Content-Length`; keep the `media.rs:61-74` streaming-abort shape under
  the new trigger).
- Evicted media re-fetches while its source remains available.

## 8. Sequencing vs Pinetree

Sequencing note (not a gate): Pinetree builds its storage lifecycle +
eviction machinery first (fetching + SHA-256 addressing already exist
there), including its shared-blob deletion fix; Birchtree then applies the
same policy concepts + store separation before its remote-media
implementation lands. Do not gate Birchtree's Phase 6 exit on another
repository — `birchtree.md` §7 Phase 6 exits on per-repo executed evidence
(two-server interop, thread completion, SSRF/body-limit tests).
