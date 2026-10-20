# Handoff: Birchtree — Admin API (Phase 7.3)

## Context (read first)

- Plan: `birchtree.md` §7 "Phase 7.3 — Administration, moderation, trends and analytics" (roles, admin API, reports queue, trends, annual reports, exit gate). Architecture rules: `AGENTS.md`.
- Contract rows: `docs/endpoint-manifest.md` "Admin API (Phase 7.3)" (~63 rows), "Reports (Phase 7.3)", "Trends, public (Phase 7.3)", "Annual reports (Phase 7.3)". Each row pins method, path, auth (`user + role`), scope family, and upstream source file. The manifest is the source of truth for what's in scope — if a route isn't rowed there, don't build it.
- Deviation register: `docs/compatibility-profile.md` (add rows if admin behavior deliberately differs; a difference not in the register is a bug).
- Reference (read-only): Mastodon v4.7.3 `app/controllers/api/v1/admin/*`, `app/controllers/api/v2/admin/accounts_controller.rb`, `Admin::PermissionsConcern`, Doorkeeper config (`admin:read`/`admin:write` families + per-resource sub-scopes). Pinned tag commit `8ff11c2b`.
- Starting state: admin routes are absent (404). `POST /api/v1/reports` already exists (stores report + forwards Flag when asked); the moderator queue does not. `create-account` CLI creates role-less accounts. Scope registry has no `admin:*` scopes. No trend data, no annual-report jobs.
- Conventions to follow: domain ← application ← adapters (domain knows no HTTP/DB shapes); one redb txn = state + indexes/counters + jobs; auth/scopes enforced centrally at the route boundary (`src/api/mastodon/auth.rs`, see manifest line 9 for the exact 401/403/422 shapes); role check runs before the scope check (manifest line 253).

## Objective

Implement Phase 7.3 to its exit gate: roles, the full admin API, the report moderation queue, trends (public reads over admin-approved items only), and annual reports — with fixture-backed tests per route and the endpoint manifest updated to match.

## Instructions (use subagents)

You MUST use subagents for parallel implementation. Suggested split (run in parallel; each owns its rows + tests + manifest updates):

- Subagent A (roles + scope plumbing): `admin`/`moderator` roles on local users, CLI grant commands extending `create-account`, `admin:read`/`admin:write` scope families + per-resource sub-scopes in the scope registry, central role-before-scope gate with upstream's forbidden response for role-less users. Confirm the permission split against upstream's permission set before building (birchtree.md proposes moderator = reports+accounts routes, admin = everything; verify, don't assume).
- Subagent B (admin accounts + reports queue): `admin/accounts*` rows (list/show/actions incl. approve/reject — note: needs registration approval modes, a Phase 2 gap — scope exactly what ships vs what stays a documented gap), `admin/reports*` moderator queue wired to the existing `POST /api/v1/reports` records.
- Subagent C (admin infra rows): domain allows/blocks, email-domain blocks, canonical email blocks (+`test`), IP blocks (enforced in the request path via trusted-proxy client IP), tags, measures/dimensions/retention (note: these POST reads take *read* scopes upstream — keep that), audit trail (proposed default: on, one record per admin write).
- Subagent D (trends + annual reports): public `GET /api/v1/trends` (+`/tags`, `/links`, `/statuses`; offset/limit paging, admin-approved items only, `/trends` is a deprecated alias of `/tags`; link trends crawl via the safe federation client), trend approval workflow + tests, annual-report generation as a durable job with state/mark-read reads.

Synthesize A+B+C+D on the shared pieces before merging: the role gate shape, the admin DTO packers (`REST::Admin::*` serializers), and the audit-trail record. No route ships without its test.

## Questions to answer along the way

- What is upstream's exact forbidden response for a token without the role vs without the scope? (Both must match; they are different cases.)
- Which `approve`/`reject` and suspension/silence behaviors are blocked on the Phase 2 gaps (approval modes, email confirmation)? Ship what's shippable; record the rest as explicit planned-gap entries, not stubs.
- IP blocks: does the enforcement point see the true client IP through the trusted-proxy config in all deployment shapes?

## Implementation rules

- No "success-shaped stubs": a route not built stays absent (404). An admin route with no data returns what upstream returns for empty (verify per route), never fabricated rows.
- Keep architecture rules per AGENTS.md: domain has no HTTP/DB shapes; application ops shared by API + ActivityPub adapters; one txn = state + outbox/job; no blobs or unbounded fan-out in txns; workers do follow-up work with idempotent retry.
- Audit trail on for every admin write (proposed default; confirm with operator if it costs a schema migration).
- **Update `docs/endpoint-manifest.md` as you go: flip each shipped row from `planned (Phase 7.3)` to `implemented (Phase 7.3)`, and correct any row whose pinned behavior turns out to be wrong (method, scope, paging, error shape).** A route that ships without its manifest row flipped does not count as done. Check the 10 stale rows found 2026-10-20 (accounts multi-fetch/lookup/search, reports, suggestions, announcements, donation campaigns — all live in `router.rs` but still marked `planned`) and fix those too.
- Append one dated log to `state/logs/YYYY-MM-DD-<topic>.md`; never rewrite past logs. Record executed evidence (`docs/logs/` entry) per phase-exit rules.
- Gate: `cargo check --all-targets` warning-free + `cargo test` (baseline 349/349 on `5e691f1` + README commit) fully green, plus a smoke script driving the role-gated flows over HTTP (401/403/422 shapes included).

## Deliverables

- Roles + CLI grants + scope families with gate tests (role-less 403 vs scopeless 403 distinguished).
- Every admin/reports/trends/annual-report route implemented with a fixture-backed test, or an explicit planned-gap entry explaining which upstream dependency is missing.
- `docs/endpoint-manifest.md` accurate to the code (no `planned` row that is live, no `implemented` row without a test).
- Compatibility-profile rows for any deliberate admin-behavior deviation.

## Non-goals

- Do not touch media transcoding (7.1), search engine (7.2), conversations, Web Push, `/api/web/*`, or federation delivery code. Do not invent admin routes not rowed in the manifest. Do not change the moderator/admin permission split without operator sign-off.
