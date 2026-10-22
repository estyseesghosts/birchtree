# 2026-10-22 — reviewer follow-ups: worker concurrency/retention + dependency audit

On `master` (`bc4a5a5`, core-ext rebase). Two subagents with disjoint file
sets (worker/jobs/ports vs Cargo lock); parent merged, cleaned up, verified,
and commits.

## 1. Background worker concurrency + general job retention

**Problem:** `src/jobs/worker.rs` claimed a batch and ran jobs sequentially,
awaiting network-bound federation ops — one slow inbox stalled everything.
The JOBS table kept every `Succeeded` row forever.

**Fix (all in this commit):**
- `run` now fans each claimed batch out over `tokio::task::JoinSet` with
  **class-specific semaphores**: `MAX_IN_FLIGHT_NETWORK = 8`
  (DeliverActivity, FederationEvent, ResolveParent, CrawlReplies,
  FetchRemoteMedia/Emoji, QuoteReverify, RelayForward, SyncFollowers,
  PreviewCardFetch, WebPushDelivery) and `MAX_IN_FLIGHT_LOCAL = 8`
  (everything else store-local). CPU-heavy arms (`TranscodeMedia`,
  `CacheEvict`/`ReconcileStorage`) bypass the async semaphores — they
  already run on the blocking thread pool. Per-job logic moved byte-identical
  into `process_one` (same claim-then-settle: `complete`/`quarantine`/`retry`;
  detached key spawn, media gauge, heartbeat preserved). Shutdown drains the
  claimed batch (checked between polls); a lost task's lease expires and is
  reclaimed. Ordering caveat documented in code: per-destination deliveries
  may interleave; all arms are idempotent and retries/cooldowns are per job
  and per destination.
- Retention: new `JobQueue::prune_succeeded(older_than_ms, limit)` in
  `src/persistence/redb/jobs.rs` (single write txn; only `Succeeded` rows
  with `due_at_ms` older than cutoff, oldest-first, bounded; no index cleanup
  needed since `complete` already unindexed). Wired into the existing
  `CacheEvict | ReconcileStorage` maintenance arm next to
  `prune_email_secrets`: `SUCCEEDED_RETENTION_MS` (7 days),
  `SUCCEEDED_PRUNE_BATCH` (500). `Quarantined` rows are never pruned
  (operator inspection surface); `complete()` semantics unchanged (row stays
  `Succeeded`, `get` works — `persistence_conformance` and `b_email_secrets`
  assert this).
- New `tests/z_worker_jobs_probe.rs` (3 tests: only-old-Succeeded pruned,
  limit honored, get-after-complete still `Succeeded`).

**Process note:** the worker subagent's `cargo fmt` reformatted the whole
tree (rustfmt 1.10 vs repo style drift — confirmed pre-existing on untouched
`actor.rs`). Parent reverted all non-functional churn (`git checkout -- .`)
and re-applied only the functional hunks above, so this diff is minimal
(+~200/−~60 in worker.rs, +~30 jobs.rs, +~8 ports.rs).

## 2. Dependency audit

- `rustls-webpki` 0.103.15 → **0.103.16** (RUSTSEC-2026-0382, CVE-2026-111943).
- `webpki-roots` 1.0.9 → **1.0.10** (RUSTSEC-2026-0381, CVE-2026-111943).
  `cargo update -p` touched only these two packages (+10/-10 lock lines);
  `Cargo.toml` unchanged.
- `blurhash` 0.1.1 kept: **decode is unreachable.** Single `blurhash::` call
  tree-wide is `encode` (`src/media/processing.rs:149`, now with a comment
  citing RUSTSEC-2023-0083); `cargo tree -i blurhash` shows only birchtree
  as a dependent; no `blurhash::decode` call or inbound decode path exists
  (remote strings never enter a decoder). No blind upgrade or suppression.

## Verification (real output, native Windows, unique target dir)

- `cargo check --all-targets`: EXIT=0. Only pre-existing warnings
  (`transcode.rs:742` unused `pid`, `remote_cache.rs:721` dead code).
- `cargo test` (full, clean rebuild): EXIT=0 — lib 220/220, every suite `ok`,
  0 failed (incl. new `z_worker_jobs_probe` 3/3; `gaps_federation_tail` 19/19
  post-TLS-bump; `phase4_1_home_cap`/`phase4_1_json_parity` long suites ok).
- `scripts/smoke-all.sh`: its `check` + full `test` phases re-ran green in
  this session; `smoke-phase1.sh` binary step still fails pre-existing on the
  Step-1 keyring refusal (unchanged by this commit; script predates it).
  Manual `--lab` binary smoke on the final tree: /health ok, /ready ready,
  create-account ok, list-emojis exit 0.
- Mid-session infra: C: hit 0 bytes free (scratch target dirs); freed 25 GB
  by deleting stale per-agent target dirs. One full-test attempt then failed
  at link with missing rlibs (target-dir corruption from the full disk);
  `cargo clean` + rebuild resolved it; the committed gate above is the clean
  rebuild.

## Open questions

1. Drain-vs-abort on shutdown: kept drain-to-settlement; abort would leave
   leases to expire (safe but slower to stop). Confirm preference.
2. Bounds (8/8) and retention (7 days/500): heuristics — suggest watching
   claim→settle latency and pruned-row counts post-deploy.
3. `smoke-phase*.sh` need a `--lab`/keyring update for the post-hardening
   tree (hardening track owns that).
4. `blurhash` 0.1.1 stays until a patched release matters for an enabled
   code path; revisit if `decode` is ever introduced (the new comment points
   at the advisory).
