# Current state

Status: 2026-10-16, Phase 1 (runtime, `redb`, IDs, configuration, bootstrap)
implemented; Phase 2 not started.

## Implemented

Repository and contracts:

- Repository layout per `birchtree.md` §3 (one Cargo package, `domain` ← `application` ← adapters)
- `AGENTS.md`, `docs/` contracts, `docs/wiki/`, append-only `state/logs/` and `docs/logs/`
- `scripts/smoke-all.sh` full gate; `.github/workflows/ci.yml` release-build CI

Runtime (Phase 1):

- Structured `tracing` logging with `RUST_LOG` filtering and secret redaction
- `GET /health` and `GET /ready` probes; unknown routes return 404
- Graceful shutdown on SIGINT/SIGTERM (accept loop drains, worker signalled and awaited)
- Configuration validation (public URL, data dir, body/upload bounds, poll interval)
- Data-dir path permissions (`0700`; world-writable dirs refused)
- Request body limits and trusted-proxy client-IP handling
- CLI: `serve`, `check-config`, `create-account` (plus test-only `crash-harness`)

IDs and clock (Phase 1):

- Mastodon-shaped snowflake IDs: 48-bit millisecond timestamp `<< 16` | 16-bit
  sequence, strictly increasing, clock-rollback safe; serialized as decimal
  strings with numeric (not lexicographic) ordering

Storage and jobs (Phase 1):

- Versioned `redb` schema + migration runner (fresh DB → v1; idempotent re-open;
  newer on-disk schema refused)
- Narrow persistence ports (`AccountStore`, `OAuthStore`, `SessionStore`,
  `JobQueue`, `ReadyCheck`); no `redb` type escapes the persistence adapter
- Atomic mutation + follow-up job commit in a single transaction (e.g. account
  create writes account + handle index + credential + `UserCreated` job)
- Durable job envelope with due/lease index, retry backoff, quarantine; in-process worker

Accounts, credentials, OAuth records (Phase 1):

- Accounts + local users; Argon2 password hashing with constant-time verification
- Basic login session for OAuth approval (hashed session tokens, 24h TTL)
- OAuth application registration records and hashed access tokens
- Registration defaults closed; no public signup/UI beyond what OAuth requires

Tests:

- `cargo test` — **45 passed, 0 failed** (29 unit + 3 `crash_recovery` + 10
  `persistence_conformance` + 3 `runtime` + 3 `secret_handling`)
- `scripts/smoke-phase1.sh` — live binary → `SMOKE_OK` (health/ready/404,
  graceful shutdown, CLI create-account, restart durability, worker handled job)

## Not yet implemented

- All Mastodon REST endpoints: OAuth (`/api/v1/apps`, token), instance discovery,
  accounts, statuses, timelines, notifications, media, streaming, federation —
  every unmapped route is explicitly out of profile (404), not a success-shaped stub
- Metrics endpoint (deferred to Phase 8)
- HTTP cursor/`Link` headers and query-bound parsing (Phase 2+)
- Email verification, MFA, password reset (Phase 2)
- Media/blob storage and processing jobs (Phase 4/6)

## Known gaps

Everything in `birchtree.md` Phases 2–8. See `docs/phase-1-exit-criteria.md`
(complete, with explicit deferrals) and `docs/phase-0-exit-criteria.md` for the
still-open route-inventory review against the pinned Mastodon source.
