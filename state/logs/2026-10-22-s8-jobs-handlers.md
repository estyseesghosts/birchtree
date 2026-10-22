# 2026-10-22 — S8 jobs worker + handlers (decomposition proposal)

## What changed
- `jobs/worker.rs`: adopted `WorkerCtx` from S0 — `process_one(ctx:
  &WorkerCtx, job)` (was 8 params + `#[allow]`), `run(ctx: WorkerCtx,
  ...)` (was 10 `Arc`s). Match arms delegate to per-kind modules;
  blocking/ordering-sensitive arms stay inline (transcode spawn_blocking,
  CacheEvict/ReconcileStorage, ProcessMedia gate, UserCreated
  post-success hook). `handle_with_transcode` call sites updated.
  ~230 lines removed from the file.
- `jobs/handlers/` (file → `mod.rs` + 8 modules): `deliver`,
  `federation_event`, `threads` (4 kinds), `remote_media` (incl.
  `parse_emoji_job`, async — matches the worker path), `media`, `mail`,
  `scheduled`, `maintenance`. Sync drain keeps explicit 6-arg
  `handle(job, store, mailer|blobs|streaming, transcode)` signatures
  (deliberate: the sync path has no federation/clock to build a
  `WorkerCtx` from, and the `None`-transcode legacy finish must keep
  working — B's `Some(&ctx.extra.transcode)` hardcoding corrected).
- `jobs/retry.rs` (new): re-exports `domain::job` scheduling constants
  (LEASE 30s, 1s→300s backoff, max attempts 5 — unchanged values).
- `handle` (sync, 5-param, used by tests) unchanged; `mail_job_fields`
  moved into `mail.rs` (single user); `notification_payload` stays.
- `main.rs`: builds `WorkerCtx` at the `run` call site (single struct
  literal instead of 7 positional args).
- Federation-group no-op arms + `DeliverActivity`/`CacheEvict` Err arms
  stay inline in `handle_with_transcode` so the original grouping and
  comments are preserved verbatim.

## Verification
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test` (full, incl. job retry/quarantine + worker tests):
  1153 passed, 0 failed (no timing-constant changes).
- `rustfmt --edition 2024 --check`: clean.
