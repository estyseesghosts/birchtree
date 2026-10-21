# 2026-10-21 — production-hardening handoff, parent session

Picked up the birchtree RSA/prod-hardening handoff mid-flight (previous
agent interrupted). Four background subagents dispatched on disjoint file
sets, each with its own `CARGO_TARGET_DIR`, no commits allowed:

- RSA provider audit: `vendor/.../rsa_provider.rs`,
  `http_signatures.rs`, `src/activitypub/key_generation.rs`, `actor.rs`,
  `tests/a_rsa_provider.rs`.
- Email/secrets: `redb/secrets.rs`, `second_factor.rs`, `accounts.rs`,
  `application/accounts.rs`, `jobs/handlers.rs` (mail arms only),
  `tests/b_email_secrets.rs`.
- Media/blob: `media/blob_store.rs`, `staging.rs`, `upload_spool.rs`,
  `upload_gate.rs`, `fetch_stream.rs`, `public_media.rs`,
  `processing.rs`, `application/media.rs`, `endpoints/media.rs`,
  `endpoints/emoji.rs`, `tests/c_blob_store.rs`, `c_media_http.rs`.
- Wiring (rate/cache/IDs/readiness/signed fetch): `rate_limits.rs`,
  `request.rs`, `endpoints/admin_infra.rs` (IP helpers),
  `application/admin_infra.rs` (`client_ip`),
  `media/cache_maintenance.rs`, `remote_cache.rs`, `reconciler.rs`,
  `storage_roots.rs`, `pooled_blob_stores.rs`, `federation/media.rs`,
  `domain/id.rs`, `job.rs`, `router/readiness.rs`,
  `tests/d_hardening.rs`, `a_signed_media_fetch.rs`.

## Parent-owned changes (this session, uncommitted)

- Step 0 fixes: `pub mod secrets` moved to `redb/mod.rs`, `#[path]`
  shim removed from `redb/jobs.rs`; `second_factor.rs` + `b_email_secrets`
  imports updated to `redb::secrets`; `c_media_http.rs` builds all three
  `NewMedia` fields (no `Default` exists).
- `jobs/handlers.rs`: `CacheEvict | ReconcileStorage` sync-drain arm now
  errors instead of `Ok(())`.
- `jobs/worker.rs`: `WorkerExtra` gains `maintenance: Arc<CacheMaintenance>`
  (spawn_blocking dispatch, `DeliveryError::Retry`), `heartbeat`, and
  `reset_token_ttl_ms`; `beat()` every loop; email-token prune (500/batch)
  runs with the maintenance chain; `last_media_job_latency_ms()` static
  fed by the transcode/process arms for the latency gauge. `Debug` impl
  dropped (`CacheMaintenance` is not `Debug`).
- Ingest fetch policy: `FedContext.remote_cache` +
  `build_config_with_remote_cache` (old `build_config` delegates with the
  default, so existing callers keep compiling); `ingest_note_with_config`
  / `ingest_checked_with_config` gate the eager `FetchRemoteMedia`
  enqueue in `fetch_at_ingest(&cfg, false)`; `activities.rs` (5 sites) and
  `objects.rs` (2 sites) thread `&data.remote_cache`. Old `ingest_note` /
  `ingest_checked` remain as default-policy wrappers (no test churn).
- `redb/federation.rs`: local private keys sealed via
  `seal_text_if_enabled("actor-key:{id}")` on write, opened (fail closed,
  reseal-rewrite) on read; new `seal_legacy_actor_keys()`; key-generation
  path already single-flights and propagates errors (no replacement keys).
- `config.rs`: `secret_keyring_file` / `secret_keyring_env` options +
  validation (mutually exclusive, non-empty); schema + example updated.
- `main.rs`: `--lab`, `--unsafe-production-override=`,
  `--secret-keyring-file=`, `--secret-keyring-env=` flags;
  `check_runtime_mode()` exit-2 after `validate()` + `startup_summary()`
  log; old lab/multiplier warnings removed; concrete
  `Arc<TimestampIdGenerator>` with `observe(highest_persisted_id())` +
  skew warn; `validate_layout` before open; `PooledBlobStores` routing
  handle; `CacheMaintenance` build/restore/seed; keyring load (production
  refuses, lab warns) + `install_secret_box` + legacy seal passes;
  `WorkerHeartbeat`, `build_router_with_probes` (storage_write via new
  `RedbStore::check_write`, worker freshness, pool writability) and
  gauges (backlog, oldest age, retried, quarantined, maintenance
  pending, cache stored/inflight bytes, media latency, heartbeat age).
- Docs: `configuration.md` (mode, rightmost-untrusted proxy rule,
  keyring + recovery warning, cache/media policy), `config-schema.json`,
  `config.example.json`.

## Verification (executed)

- `cargo check --all-targets --locked`: baseline had one failure class
  (`Arc<RedbStore>: SecondFactorStore` unsatisfied, 9 sites in
  `b_email_secrets.rs` — email agent's file, left for them). After parent
  edits, remaining lib errors are in `application/admin_infra.rs:231`
  (wire agent mid-edit, left alone). No errors in any parent-owned file.
- Parent warnings fixed (unused `JobQueue` / `SecondFactorStore` imports
  in `worker.rs`). Remaining warning (`transcode.rs` unused `pid`) is in
  the media agent's file.
- Full `cargo test` not yet green; pending agent completion. Nothing
  committed (per working rules: small per-WP commits once green).

## Open / for the agents or next session

- Reconciliation-status gauge is approximated as
  `queue_maintenance_pending` (non-terminal evict/reconcile jobs); no
  fabricated series.
- `seal_legacy_actor_keys` is called from `main.rs` startup rather than
  folded into `seal_legacy_secrets` (that fn is email-agent-owned);
  folding it in is a follow-up.
- Skew warn uses the code constant `SKEW_WARN_THRESHOLD_MS` (60s), not
  the handoff's approximate 300s.
- `ffmpeg_gate`/`configure_media_work_limit` wiring skipped (optional,
  no config knob exists).
- Not done: WP7/WP10/WP12, Step 2/3 remainder, live interop, load tests.

## Update — baseline green (after wire-agent report)

- Restored `IdGenerator` trait import in `main.rs` (wire agent's
  one-line fix; `observe()` needs the trait in scope).
- `cargo check --all-targets --locked` now passes (only warnings, both
  in agent-owned files: `transcode.rs` unused `pid`, `remote_cache.rs`
  unused test helper `full_entry`).
- Wire agent's results (uncommitted): rightmost-untrusted chain rule in
  `request.rs` + both `admin_infra` `client_ip` helpers (application
  layer mirrors locally, no adapter import); `should_warm` honors
  `mode == Warm`; `fetch_at_ingest` unified; stale unsigned-fetch docs
  corrected; `SKEW_WARN_THRESHOLD_MS` added; lib tests 220/220 green in
  its target dir. Still not-done: `d_hardening` + `a_signed_media_fetch`
  integration runs, v22 WebAuthn backfill migration, runtime
  timeout/SSRF live behavior.
- RSA agent's results (uncommitted, style-only): `cargo tree -i rsa`
  empty, OpenSSL cross-verification of signing (byte-identical
  2048-bit signature, SPKI re-derivation matches). Still not-done:
  `a_rsa_provider` cargo run (its sandbox lacked a linker).

## Update — media suite 9/10, one real bug fixed

- `c_media_http`: 9 passed, 1 failed:
  `oversized_field_is_aborted_before_the_full_payload_is_resident`
  (server pulled all 2048x16KiB before aborting a 64KiB cap).
- Root cause: multer 3.1's `StreamBuffer::poll_stream` drains every
  immediately-ready frame in one poll loop, and the endpoint fed it the
  raw request body — so a fast/always-ready sender makes the whole part
  resident before `spool_field`'s cap trips. (With real network pacing
  the spool aborted early; the always-ready test stream exposed it.)
- Fix (parent, `endpoints/media.rs` `multipart()`): wrap the body in a
  one-frame-per-wake pacing adapter before axum's `Multipart` parser, so
  delivery is incremental and the spool cap aborts after roughly the
  file cap. Upload paths only; one extra task wake per frame.
- Re-run of `c_blob_store` + `c_media_http` in flight.

## Update — Windows linker rot, full rebuild

- The re-run failed to *link* (`undefined reference` to the crate's own
  symbols, including a normal `link_header_with` fn): stale incremental
  artifacts under `target/`, same rot the RSA agent reported. Not caused
  by the pacing change (which compiles clean).
- Ran `cargo clean -p birchtree` (removed 27k files / 45GB of stale
  artifacts) and relaunched the two suites in background; full dep
  rebuild in progress. Toolchain note for the record: native WinLibs
  `ld` + incremental cache on this box goes stale; clean rebuild is the
  workaround.

## Update — media suites 17/17 green (exit 0)

- `c_blob_store`: 7/7. `c_media_http`: 10/10, including the previously
  failing `oversized_field_is_aborted_before_the_full_payload_is_resident`.
  The pacing fix is confirmed (the earlier exit-code-1 was a `tail`-pipe
  artifact; redirect-to-file rerun exits 0).
- Full `cargo test` launched in background next; per-WP commits after it
  goes green (Step 0.4).

## Update — full suite findings

- Everything before `crash_recovery` passed: lib 220, `a_rsa_provider`
  9 (the RSA agent's missing cargo run is now covered),
  `a_signed_media_fetch` 6, `avatar_upload` 5, `b_email_secrets` 19,
  `c_blob_store` 7, `c_media_http` 10, `config_file` 6.
- `crash_recovery` 1/3: my `startup_summary()` log line broke the
  harness protocol (child prints `OPENED`/`COMMITTED` to stdout; the
  test asserted on the first line). Fixed the test, not the log line:
  `read_line` now skips non-marker lines (bounded at 200) — stdout log
  lines are legitimate output. Re-run in flight; full suite with
  `--no-fail-fast` after.
