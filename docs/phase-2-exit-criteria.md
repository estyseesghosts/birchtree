# Phase 2 — exit criteria checklist

Source: `birchtree.md` §9, Phase 2. Contracts: `docs/endpoint-manifest.md`,
`docs/domain-contracts.md`, `docs/application-ports.md`,
`docs/compatibility-profile.md`.

**Exit:** a real Mastodon-compatible client can register an app, register an
account when policy allows, and obtain a user token; PKCE/replay/redirect/scope/
app-token/registration tests pass; account/instance DTO snapshots match the
pinned contract.

## OAuth

- [x] `POST /api/v1/apps`: confidential application registration returning
  `CredentialApplicationSerializer` (one-time `client_secret`,
  `client_secret_expires_at: 0`). Evidence: `tests/phase2_oauth.rs`,
  `scripts/smoke-phase2.sh`.
- [x] `POST /oauth/token` `authorization_code` grant with PKCE `S256`; one-time
  codes; exact redirect-URI matching; app-identity binding.
- [x] `POST /oauth/token` `client_credentials` grant; requested scopes must be a
  subset of the app's registered scopes (`invalid_scope` otherwise).
- [x] `refresh_token` is reported `unsupported_grant_type` (Mastodon issues
  non-expiring access tokens by default).
- [x] `POST /oauth/revoke`: blank/unknown token → `200 {}`; a token owned by
  another client → `403 unauthorized_client`.
- [x] Browser `GET`/`POST /oauth/authorize`: server-rendered login + consent,
  hashed session cookie, `state` round-trip, `access_denied` on deny.
- [x] Scope enforcement: token satisfies a required list if it carries *any*
  accepted scope (broad OR granular), matching Doorkeeper `any?`.
- [x] Error shapes: missing/invalid token `401 {"error":"The access token is
  invalid"}`; insufficient scope `403 {"error":"This action is outside the
  authorized scopes"}`; user endpoint with app-only token `422 {"error":"This
  method requires an authenticated user"}`; registration with a user token
  `403 {"error":"This method requires an client credentials authentication"}`.

## Registration and accounts

- [x] `POST /api/v1/accounts`: requires an app-only token with `write` or
  `write:accounts`; returns a Doorkeeper token response (user token scoped to
  the registering application).
- [x] Policy gate: registration closed → `403 {"error":"Forbidden"}` (mirrors
  `check_enabled_registrations`). Registration does not depend on the CLI
  `create-account`, which stays available when public registration is closed.
- [x] Validation: username shape/uniqueness, email presence, password length,
  `agreement`. Duplicate username → `422` with
  `details.username = [{ error: ERR_TAKEN, ... }]`.
- [x] Mail-sender port (`MailSender`) with a logging adapter; a
  `SendConfirmationEmail` job is committed in the **same** transaction as the
  account (`tests/persistence_conformance.rs`).
- [x] Account, credential and relationship DTOs match the pinned serializers.
- [x] `GET /api/v1/accounts/{id}` (public, 404 on unknown), `GET
  /api/v1/accounts/verify_credentials`, `PATCH
  /api/v1/accounts/update_credentials` (incl. `source[quote_policy]`), `GET
  /api/v1/accounts/relationships`.

## Instance and preferences

- [x] `GET /api/v1/instance` (`REST::V1::InstanceSerializer`) and `GET
  /api/v2/instance` (`REST::InstanceSerializer`, no top-level `stats`/`urls`).
- [x] `GET /api/v1/preferences` incl. quote-policy defaults.
- [x] Config-derived `ServerInfo` (title, description, contact, streaming URL,
  registration state) assembled once at startup.

## Cross-cutting

- [x] Request form parsing (`api/forms.rs`) for OAuth and REST bodies.
- [x] Optional-auth behavior (`Auth` / `OptionalAuth` extractors).
- [x] Rate-limit middleware and `X-RateLimit-*` headers (`api/mastodon/rate_limits.rs`).
- [x] Pagination widget (`api/mastodon/pagination.rs`) with numeric cursor
  semantics; wire-up to paginated endpoints lands in Phase 3.
- [x] Schema v2 migration adds the `AUTHORIZATION_CODES` table; v1→v2 reopen is
  idempotent and preserves data.

## Evidence (executed)

- `cargo check --all-targets` — green.
- `cargo test` — **93 passed, 0 failed**: 45 unit; 3 `crash_recovery`; 13
  `persistence_conformance`; 3 `runtime`; 3 `secret_handling`; 13
  `phase2_oauth`; 13 `phase2_api` (see `docs/logs/`).
- `scripts/smoke-phase2.sh` — live binary: app registration, client-credentials,
  app-only rejection, account registration, `verify_credentials`, preferences,
  browser authorization-code + PKCE S256, single-use code, revoke, restart with
  registration closed → `SMOKE_PHASE2_OK`.
- `scripts/smoke-all.sh` — full gate ends `smoke-all: green`.

## Open / deferred (explicit)

- **Email confirmation is not enforced.** No SMTP transport is configured;
  `MailSender` ships a logging adapter and the `verify_credentials`/`GET
  /api/v1/accounts/{id}` endpoints do **not** gate on `email_verified` (Mastodon
  would 404/403 unconfirmed accounts). The confirmation job is committed and
  delivered to the adapter; enforcement waits for a real mail transport.
- **Invite / approval registration modes are not implemented.** Only
  `registrations_open` (open vs closed) is modeled; `regulations_mode` values
  `approved` and invite-only, `require_invite_text`, and the `reason` field are
  accepted/ignored.
- **Account profile fields not persisted:** `discoverable`, `indexable`,
  `hide_collections`, `bot`, `attribution_domains`, and `fields_attributes` are
  accepted but not stored (no columns yet); the DTOs emit contract-correct
  defaults. Recorded as a gap.
- **`GET /api/v1/accounts/lookup` and `/search` are not implemented** (Phase 3).
- **Metrics are disabled** (Phase 8); `usage`/`stats` counts are structural (0
  or account count).
- **Media/avatar/header serving is Phase 4**; DTOs use the conventional missing
  asset paths.
- Registration `locale`, `time_zone`, `date_of_birth` are accepted/ignored
  (min-age policy not implemented).

## Status 2026-10-20 (freshness pass, worker W1)

Three "Open / deferred" bullets above are stale at HEAD and are superseded,
not rewritten: account profile fields ARE persisted now (bot, discoverable,
indexable, hide_collections, fields/attribution — parsed in
`src/api/mastodon/endpoints/accounts.rs:221-271,416-430`, shipped by the
2026-10-20 fix round, `docs/fix-plan.md` P2 `#11d`); `GET
/api/v1/accounts/lookup` and `/search` ARE implemented
(`src/api/mastodon/router.rs:66-67`); invite-gated registration IS
implemented (HTTP registration consumes invites via
`register_account_with_invite`, `src/api/mastodon/endpoints/accounts.rs:112`;
`GET /invite/:invite_code` at `src/api/mastodon/router.rs:84`) and approval
mode exists (`--registration-require-approval`,
`src/main.rs:113-114`, with admin approve/reject at
`src/api/mastodon/router.rs:313-314`). Email-confirmation enforcement and
min-age policy remain as stated. Metrics remain Phase 8 work.
