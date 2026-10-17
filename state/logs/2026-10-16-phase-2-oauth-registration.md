# State log — 2026-10-16 Phase 2: OAuth, registration and exact read APIs

Supersedes nothing. Previous session log: `2026-10-16-phase-1-runtime-redb-ids.md`.

## Goal

Implement Phase 2 of `birchtree.md` §7/§9: `POST /api/v1/apps`,
authorization-code + PKCE S256, client credentials, token issue/revoke,
session/consent page, scopes and per-route authorization;
`POST /api/v1/accounts` registration with policy gates and a mail-sender port;
instance discovery (`/api/v1/instance`, `/api/v2/instance`), preferences,
account read/update/relationships DTOs; and exact error shapes, form parsing,
pagination/rate-limit headers before a large endpoint surface.

## What changed

OAuth:

- `src/domain/oauth.rs` — added `AuthorizationCode` (bound to app/user/redirect
  URI/scopes/PKCE, `is_redeemable_at`); `AccessToken::is_app_only`.
- `src/application/auth.rs` — `AUTH_CODE_TTL_MS`, `issue_client_credentials_token`,
  `create_authorization_code`, `CodeExchange`, `exchange_authorization_code`
  (one-time use, expiry, app identity, exact redirect-URI match, PKCE S256).
- `src/application/crypto.rs` — `pkce_s256_challenge` / `pkce_verify_s256`
  (RFC 7636 appendix-B vector test).
- `src/api/oauth/token.rs` — `POST /oauth/token` (`authorization_code`,
  `client_credentials`; Doorkeeper error shape; `no-store`/`no-cache`) and
  `POST /oauth/revoke` (blank/unknown → `200 {}`; wrong client → 403).
- `src/api/oauth/{authorization,consent_page,pkce,scopes}.rs` — browser
  authorize (login + consent), PKCE validation, scope parsing/subset checks.
- `src/api/forms.rs` — hand-written form-urlencoded parser (no new deps).

Persistence:

- `src/persistence/redb/schema.rs` — `AUTHORIZATION_CODES` table; `SCHEMA_VERSION = 2`.
- `src/persistence/redb/migrations.rs` — step v2 creates the table.
- `src/persistence/redb/oauth.rs` — `app_by_id`, `create_authorization_code`,
  `authorization_code_by_plaintext`, `redeem_authorization_code` (marks the code
  used and writes the token in one transaction; replays cannot mint a second token).
- `src/persistence/redb/accounts.rs` — `update_account_profile`,
  `update_user_settings`; confirmation-mail job committed with the account.

Application:

- `src/application/accounts.rs` — `RegisterAccountCommand`,
  `RegisteredAccount`, `register_account` (policy gates + token issuance).
- `src/application/ports.rs` — `NewLocalAccount::send_confirmation_email`,
  `NewAuthCode`, `UserSettingsUpdate`, `AccountProfileUpdate`, `MailSender`;
  extended `AccountStore`/`OAuthStore`.

Mail/jobs:

- `src/mail/mod.rs` — `LoggingMailSender` adapter.
- `domain/job.rs` + `jobs/{handlers,worker}.rs` — `JobKind::SendConfirmationEmail`
  and its handler; the mailer is threaded through the worker.

REST endpoints/API:

- `src/api/mastodon/{errors,auth,dto,pagination,rate_limits}.rs` — Mastodon error
  shapes and `Auth`/`OptionalAuth` extractors; account/credential/app/instance/
  preferences/relationship/token DTO packers; numeric-cursor `Link` builder;
  rate-limit middleware + `X-RateLimit-*` headers.
- `src/api/mastodon/endpoints/{mod,apps,accounts,instance,preferences}.rs` —
  `POST /api/v1/apps`; `POST /api/v1/accounts`; `GET /api/v1/accounts/{id}`,
  `verify_credentials`, `PATCH update_credentials`, `relationships`;
  `GET /api/v1/instance`, `/api/v2/instance`, `/api/v1/preferences`.
- `src/api/mastodon/{mod,router}.rs` — `ServerInfo` (from `Config`), full route
  table (static paths before `{id}`).
- `src/{config,main}.rs` — site title/description/contact config; `--registration-open`
  and `--site-*` flags; `ServerInfo`, `LoggingMailSender`, mailer-aware worker.

Tests/scripts:

- `tests/phase2_oauth.rs` (13), `tests/phase2_api.rs` (13).
- `tests/common/mod.rs` — `TestServer` + HTTP request/response helpers;
  `tests/persistence_conformance.rs` +3 (single-use code, concurrent redeem,
  confirmation-mail job); `tests/runtime.rs` updated for `ServerInfo`.
- `scripts/smoke-phase2.sh` (app → client-credentials → registration → PKCE →
  revoke → closed-registration); wired into `scripts/smoke-all.sh`.

Docs:

- `docs/phase-2-exit-criteria.md` (new checklist, with explicit deferrals).
- Updated `docs/{endpoint-manifest,compatibility-profile,domain-contracts,application-ports,README}.md`,
  `docs/wiki/{Home,Current-State,Roadmap,Feature-Matrix,About}.md`, `AGENTS.md`.

## Evidence

Executed on the release-relevant baseline:

- `cargo check --all-targets` — green, no warnings.
- `cargo test` — **93 passed, 0 failed**: 45 unit; 3 `crash_recovery`; 13
  `persistence_conformance`; 3 `runtime`; 3 `secret_handling`; 13 `phase2_oauth`;
  13 `phase2_api`.
- `scripts/smoke-phase2.sh` — live binary via curl: app registration,
  client-credentials, app-only rejection (422), registration + `verify_credentials`,
  preferences, browser authorization-code + PKCE S256, single-use code (400),
  revoke (401 on use), closed registration (403) → `SMOKE_PHASE2_OK`.
- `bash scripts/smoke-all.sh` — `smoke-all: green` (check + test + phase1 +
  phase2).

Full captured output: `docs/logs/2026-10-16-phase-2-verification.md`.

## Open questions / deferrals

- **Email confirmation is not enforced** (no SMTP transport). The
  `SendConfirmationEmail` job commits with the account and is handled by a
  logging adapter; endpoints do not gate on `email_verified`. Recorded in
  `phase-2-exit-criteria.md`.
- Invite/approved registration modes, `require_invite_text`, and `reason` are
  accepted/ignored; only open/closed is modeled.
- `discoverable`/`indexable`/`hide_collections` are accepted by
  `PATCH update_credentials` but not persisted (no columns yet).
- `GET /api/v1/accounts/lookup` and `/search` are not implemented (Phase 3).
- Metrics disabled (Phase 8). Media serving is Phase 4 (DTOs use placeholder
  asset paths).
- `cargo fmt`/`clippy` remain unavailable and are not part of the gate.

## Next step

Phase 3: local statuses, relationships and interaction semantics (create/read/
edit/delete, source/context/history, visibility policy, follow/block/mute/
favourite/bookmark, relationships write endpoints, and wiring the pagination
widget into paginated collections).
