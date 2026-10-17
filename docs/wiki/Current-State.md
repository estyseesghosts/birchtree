# Current state

Status: 2026-10-16, Phase 2 (OAuth, registration, exact read APIs) implemented;
Phase 3 not started.

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
- CLI: `serve`, `create-account` (plus test-only `crash-harness`)

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

OAuth, registration and read APIs (Phase 2):

- `POST /api/v1/apps` — app registration, one-time client secret
- `GET`/`POST /oauth/authorize` — server-rendered login + consent, session cookie
- `POST /oauth/token` — `authorization_code` (PKCE S256) and `client_credentials`
- `POST /oauth/revoke`
- `POST /api/v1/accounts` — registration gated by policy, returns a user token;
  requires an app-only token (Mastodon semantics)
- `GET /api/v1/accounts/{id}`, `GET /api/v1/accounts/verify_credentials`,
  `PATCH /api/v1/accounts/update_credentials` (incl. `source[quote_policy]`),
  `GET /api/v1/accounts/relationships`
- `GET /api/v1/instance`, `GET /api/v2/instance`, `GET /api/v1/preferences`
- Scope enforcement (broad OR granular), Mastodon error shapes, rate-limit
  headers, `Auth`/`OptionalAuth` extractors, numeric-cursor pagination widget
- Schema **v2**: `authorization_codes` table; one-transaction
  redeem-and-issue; `SendConfirmationEmail` job committed with registration

Tests:

- `cargo test` — **93 passed, 0 failed** (45 unit + 3 `crash_recovery` + 13
  `persistence_conformance` + 3 `runtime` + 3 `secret_handling` + 13
  `phase2_oauth` + 13 `phase2_api`)
- `scripts/smoke-phase1.sh` — live binary → `SMOKE_OK`
- `scripts/smoke-phase2.sh` — live app registration → PKCE → user token → revoke
  → `SMOKE_PHASE2_OK`

## Not yet implemented

- Phase 3+ REST endpoints: statuses, timelines, notifications, media, streaming,
  federation, lookup/search, paginated account collections — every unmapped
  route is explicitly out of profile (404), not a success-shaped stub
- Email-confirmation enforcement (no SMTP transport; `MailSender` is a logging
  adapter and endpoints do not gate on `email_verified`)
- Invite/approval registration modes (only open/closed is modeled)
- `discoverable`/`indexable`/`hide_collections` persistence (accepted, not stored)
- Metrics endpoint (deferred to Phase 8)

## Known gaps

Everything in `birchtree.md` Phases 3–8. See
`docs/phase-2-exit-criteria.md` (complete, with explicit deferrals),
`docs/phase-1-exit-criteria.md`, and `docs/phase-0-exit-criteria.md` for the
still-open route-inventory review against the pinned Mastodon source.
