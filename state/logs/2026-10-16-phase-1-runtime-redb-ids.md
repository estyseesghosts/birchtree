# State log — 2026-10-16 Phase 1: runtime, `redb`, IDs, configuration, bootstrap

Supersedes nothing; Phase 0 log is `2026-10-16-project-init.md`.

## Goal

Implement Phase 1 of `birchtree.md` (§7): structured logs/tracing, health/readiness,
graceful shutdown, configuration validation, path permissions, body limits,
trusted-proxy handling, release-build CI; Mastodon-shaped decimal-string IDs and
`Clock`; versioned `redb` schema, narrow persistence ports, migrations and a
conformance suite; accounts/local users, Argon2 password hashing, a basic login
session for OAuth approval, CLI `create-account`, OAuth application/token records
with hashing, and durable jobs. No public signup/UI beyond what OAuth requires.

## What changed

Runtime:

- `src/observability.rs` — `tracing`/`EnvFilter` init, `redact_authorization` secret redaction.
- `src/config.rs` — validated config: public URL scheme, non-empty data dir, body/upload bounds, poll interval; `prepare_data_dir` creates `0700`, refuses world-writable dirs.
- `src/main.rs` — CLI (`serve`, `check-config`, `create-account`, test-only `crash-harness`), graceful SIGINT/SIGTERM shutdown, request body limit, worker startup/stop.
- `src/api/mastodon/{router,mod,request}.rs` — `GET /health`, `GET /ready`, 404 for unmapped routes, trusted-proxy client IP.
- `.github/workflows/ci.yml` — check, test, release build, Phase 1 smoke, `cargo audit`.

IDs/clock:

- `src/domain/id.rs` — 48-bit ms timestamp `<< 16` | 16-bit sequence; strictly increasing, clock-rollback safe; serde decimal-string wire form with numeric ordering; `TimestampIdGenerator`, `Clock` port.

Storage/ports/jobs:

- `src/persistence/redb/{schema,migrations,mod}.rs` — versioned schema + migration runner (fresh → v1, idempotent re-open, newer schema refused); atomic mutation + follow-up job in one transaction.
- `src/persistence/redb/{accounts,oauth,jobs}.rs` — port implementations; due/lease job index keyed by 16-byte `(timestamp_be || id_be)` so crashed workers are reclaimable without a recovery scan.
- `src/application/{ports,context}.rs` — narrow ports (`AccountStore`, `OAuthStore`, `SessionStore`, `JobQueue`, `ReadyCheck`, bundled `Store`); no `redb` type escapes.
- `src/domain/job.rs`, `src/jobs/{worker,handlers,types,queue}.rs` — durable job envelope, retry backoff, quarantine, in-process worker.

Accounts/credentials/OAuth:

- `src/domain/{account,user,oauth}.rs`, `src/application/{accounts,auth,crypto}.rs` — account/local-user split, Argon2 hashing + constant-time verify + dummy verify, login session (hashed tokens, 24h TTL), OAuth app/token records with hashed tokens.

Tests and scripts:

- `tests/persistence_conformance.rs` (10), `tests/crash_recovery.rs` (3), `tests/runtime.rs` (3), `tests/secret_handling.rs` (3), `tests/common/mod.rs`.
- `scripts/smoke-phase1.sh`; `scripts/smoke-all.sh` extended to run it.

Docs:

- `docs/phase-1-exit-criteria.md` (new checklist); updated `docs/README.md`, `docs/architecture.md`, `docs/domain-contracts.md`, `docs/application-ports.md`, `docs/persistence-conformance.md`, `docs/testing.md`.
- `docs/wiki/{Home,Current-State,Roadmap,Feature-Matrix}.md`; `AGENTS.md` current phase.

## Evidence

Executed on the release-relevant baseline:

- `cargo check --all-targets` — green.
- `cargo test` — **45 passed, 0 failed**: 29 unit; 3 `crash_recovery`; 10
  `persistence_conformance`; 3 `runtime`; 3 `secret_handling`.
- `scripts/smoke-phase1.sh` — live binary: health/ready/404, graceful SIGTERM,
  CLI `create-account`, restart durability, worker handled `UserCreated` job →
  `SMOKE_OK`.
- `bash scripts/smoke-all.sh` — `smoke-all: green` (check + test + smoke).

Full output is recorded in `docs/logs/2026-10-16-phase-1-verification.md`.

## Open questions / deferrals

- Metrics endpoint and performance thresholds — Phase 8.
- HTTP cursor/`Link` headers and query-bound parsing — Phase 2+ (numeric cursor
  comparison semantics are already tested).
- Email verification, MFA, password reset — Phase 2.
- Media/blob storage and processing; `ProcessMedia`/`DeliverActivity` job kinds
  exist but handlers return an explicit "not implemented" error and are never
  enqueued before Phase 4/6.
- Phase 0 route-inventory review against `config/routes/api.rb` remains open; it
  did not gate Phase 1.
- `cargo fmt`/`clippy` are not installed locally and CI omits them, so formatting
  and lint checks are not part of the gate.

## Next step

Phase 2: `POST /api/v1/apps`, authorization-code/PKCE S256, client credentials,
token issue/revoke, session/consent page, scopes and per-route authorization;
`POST /api/v1/accounts` registration with policy gates; instance discovery and
core account/instance DTOs; exact error shapes, pagination and rate-limit headers.
