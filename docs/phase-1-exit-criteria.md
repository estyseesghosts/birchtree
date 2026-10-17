# Phase 1 — exit criteria checklist

Source: `birchtree.md` §9, Phase 1. Contracts: `docs/` (Phase 0 drafts; see
`docs/phase-0-exit-criteria.md`).

**Exit:** fresh install and restart preserve identity/data; unique-handle races
are safe; IDs serialize/cursor correctly; storage conformance and crash-recovery
tests prove primary mutation + job atomicity; secret handling tests pass.

## Runtime

- [x] Structured logging via `tracing` (`src/observability.rs`);
  `RUST_LOG`-controlled `EnvFilter`; secret redaction helper
  (`redact_authorization`).
- [ ] Metrics — deferred; no metrics endpoint yet (explicit gap, Phase 8
  performance qualification).
- [x] Health and readiness probes (`GET /health` → `ok`, `GET /ready` → checks
  the store can open a read). Evidence: `tests/runtime.rs`.
- [x] Graceful shutdown on SIGINT/SIGTERM; the job worker is signalled and
  awaited before exit (`src/main.rs`). Evidence: `scripts/smoke-phase1.sh`.
- [x] Configuration validation (`src/config.rs`): public URL scheme, non-empty
  data dir, body/upload bounds, poll interval.
- [x] Path permissions: new data dirs are created `0700`; world-writable
  directories are refused (`prepare_data_dir`; unit test
  `rejects_world_writable_data_dir`).
- [x] Request body limits via `DefaultBodyLimit` from `max_body_size`.
- [x] Trusted-proxy handling for client IP (`api/mastodon/request.rs`; forwarded
  header honoured only for trusted peers).
- [x] Release-build CI (`.github/workflows/ci.yml`: check, test, release build,
  Phase 1 smoke, `cargo audit`).

## IDs, clock, cursors

- [x] Mastodon-shaped snowflake: 48-bit millisecond timestamp `<< 16` plus
  16-bit sequence, strictly increasing, clock-rollback safe
  (`src/domain/id.rs`).
- [x] Decimal-string wire form via `serde`; numeric ordering internally
  (`id_serializes_as_decimal_string`, `numeric_order_matches_time_order_not_lexicographic`).
- [x] `Id::from_parts` / `timestamp_ms` / `sequence` matching `Snowflake.id_at`.
- [x] `Clock` port and deterministic test clock.
- [ ] API cursor/link serialization — the numeric comparison semantics are
  tested here; the HTTP `Link` header and query-bound behavior land with the
  paginated endpoints in Phase 2+.

## Storage (`redb`)

- [x] Versioned schema and migration runner (`schema.rs`, `migrations.rs`):
  fresh DB migrates to v1; re-open is idempotent; a newer on-disk schema is
  refused.
- [x] Narrow persistence ports (`application/ports.rs`: `AccountStore`,
  `OAuthStore`, `SessionStore`, `JobQueue`, `ReadyCheck`, bundled `Store`). No
  `redb` type escapes `persistence::redb`.
- [x] Atomic mutation + job commit: `create_local_account` writes the account,
  handle index, credential, and the `UserCreated` job in one transaction
  (`insert_job`).
- [x] Common conformance suite: `tests/persistence_conformance.rs`.
- [x] Storage probes: `src/persistence/redb/mod.rs` unit tests.

## Accounts, credentials, jobs

- [x] Accounts + local users with an `acct` split from credentials
  (`domain/account.rs`, `domain/user.rs`).
- [x] Argon2 password hashing (`application/crypto.rs`); constant-time
  verification; dummy verify for unknown handles.
- [x] Basic login session for OAuth approval (`SessionStore`, hashed session
  tokens, 24h TTL).
- [x] CLI `create-account` (`src/main.rs`); verified against a fresh data dir.
- [x] OAuth application registration records and hashed access tokens
  (`domain/oauth.rs`, `OAuthStore`).
- [x] Durable jobs: envelope with due/lease index, retry backoff, quarantine
  (`domain/job.rs`, `persistence/redb/jobs.rs`, `jobs/`), plus an in-process
  worker (`jobs/worker.rs`).
- [x] No public signup/UI beyond what OAuth requires; registration flag defaults
  closed.

## Evidence (executed)

- `cargo check --all-targets` — green.
- `cargo test` — **45 passed, 0 failed**: 29 unit; 3 `crash_recovery`; 10
  `persistence_conformance`; 3 `runtime`; 3 `secret_handling` (see
  `docs/logs/`).
- `scripts/smoke-phase1.sh` — live binary: health/ready/404, graceful SIGTERM,
  CLI `create-account`, restart durability → `SMOKE_OK`.

## Open / deferred (explicit)

- Metrics endpoint and performance thresholds (Phase 8).
- HTTP cursor/`Link` headers and query-bound parsing (Phase 2).
- Email verification, MFA, password reset (Phase 2).
- Media/blob storage and processing jobs are declared in the job enum but the
  handlers return an explicit "not implemented" error until Phase 4/6; the
  kinds are not enqueued before then.
- Phase 0 route-inventory review against `config/routes/api.rb` remains open
  (`docs/phase-0-exit-criteria.md`); Phase 1 did not depend on it.
