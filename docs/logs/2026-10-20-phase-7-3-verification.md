# 2026-10-20 — Phase 7.3 verification (admin API, roles, trends, annual reports)

Append-only evidence. Gate: `birchtree.md` §7 Phase 7.3 exit + `HANDOFF_admin-api.md`
deliverables. Implementation was split across four parallel subagents (session
logs: `state/logs/2026-10-20-admin-infra-c.md`,
`state/logs/2026-10-20-phase7-3-admin-accounts-reports-subagent-b.md`,
`state/logs/2026-10-20-trends-annual-reports.md`); integration, router wiring,
and verification by the main session (session log:
`state/logs/2026-10-20-phase-7-3-integration.md`).

Reference: Mastodon v4.7.3, tag `8ff11c2b` (read-only at
`~/fedifedifedi/mastodon`).

## 1. Upstream findings confirmed against source (no live v4.7.3 run)

- Role check (`Admin::PermissionsConcern#require_moderator_or_admin_permissions`)
  runs before each controller's `authorize_if_got_token!` scope check; a role
  failure renders `403 {"error":"Forbidden"}` (`ErrorResponses#forbidden` →
  `respond_with_error(403)` → `{ error: Rack::Utils::HTTP_STATUS_CODES[403] }`),
  while a scope failure renders Doorkeeper's `403 {"error":"This action is
  outside the authorized scopes"}`.
- `Admin::BasePolicy#access?` admits any moderation/administration/devops flag;
  birchtree's two-role split (moderator = accounts+reports admin areas, admin =
  everything) is a deliberate simplification (D35).
- Public trends serve approved items only (`Trends.*.query.allowed`); admin
  trends `:id`s are numeric row ids (`Tag.find(params[:id])`, numeric
  `REST::TagSerializer#id`). birchtree first emitted the tag *name* as `id`;
  fixed to the numeric flag id before the gate run.
- Annual-report `:id` is the year (`find_by!(account_id:, year:)`); `generate`
  answers `head 202` (empty) while running and `render_empty` (`200 {}`)
  otherwise — matched.
- Reports/admin filter flags use `.present?`/boolean-cast semantics: a bare
  `?suspended`/`?resolved` does NOT filter upstream. Tests were fixed to send
  explicit `=true` rather than codify the bare-flag behavior.

## 2. `cargo check --all-targets` (warning-free)

```
cargo check --all-targets 2>&1 | grep -cE "^(warning|error)"
0
```

## 3. `cargo test` (full suite)

```
passed=401 failed=0
```

(no `FAILED`, no `failures:` lines). Includes the five new Phase 7.3 targets:
`phase7_3_roles` (16), `phase7_3_admin_accounts_reports` (8),
`phase7_3_admin_infra` (11), `phase7_3_trends` (10),
`phase7_3_annual_reports` (3).

## 4. Bugs found by the gate run and fixed

1. **Trend tag `id` was the tag name** (see §1). Upstream uses the numeric Tag
   id. Fix: `approved_tags` returns flag rows; both serializers emit the flag
   id. Test round-trips approve/reject by that id.
2. **`account_year_key` byte order**: `year as u32` sorts `i32::MIN` above real
   years, emptying the `MIN..=MAX` pending-index range. Fix: flip the sign bit
   (`year ^ i32::MIN`). Found by the annual-report index test.
3. **`claim_due(u64::MAX, …)` overflow** in the annual-report test helper
   (`now_ms + LEASE_MS`). Fix: helper passes the real clock time (the codebase
   convention), not a core-code change.
4. **Role-gate order vs audit of `require_admin`**: none — the helper already
   enforced `require_user` (422) → role (403 `Forbidden`) → scope (403 scopes
   message), verified by matrix tests.
5. **Privacy-protective deviation (D41)**: an admin-approved non-public status
   is served only to viewers who may fetch it (`show_status`), never to
   anonymous/unauthorized viewers. Upstream's trends query has no visibility
   re-check and would leak it.

## 5. HTTP smoke over the role gates (live debug server)

Instance: `target/debug/birchtree --data-dir=/tmp/bt73
--public-url=http://127.0.0.1:4311 --listen=127.0.0.1:4311 --registration-open`;
roles via `create-account --role=` / `grant-role`. Script: `/tmp/smoke-73.sh`.

| Check | Result |
|---|---|
| `GET /api/v1/admin/accounts` no token → `401 {"error":"The access token is invalid"}` | ok |
| same, app-only token → `422 {"error":"This method requires an authenticated user"}` | ok |
| same, role-less user token (broad admin scopes) → `403 {"error":"Forbidden"}` | ok |
| same, moderator token → `200` admin account rows | ok |
| `GET /api/v1/admin/domain_blocks` moderator → `403 Forbidden`; admin → `200` | ok |
| `GET /api/v1/trends/tags` anonymous → `200` | ok |
| `GET /api/v1/admin/reports` moderator → `200` | ok |
| `POST /api/v1/admin/measures` admin (read scope) → `200` | ok |

**9 passed, 0 failed.**

## 6. Contract updates shipped with this phase

- `docs/endpoint-manifest.md`: all 7.3 rows flipped to `implemented (Phase
  7.3)` (trends public 4, annual reports 5, admin accounts 10 + v2, reports
  queue 8, domain/email/IP infra 16, canonical+tags 9, trend approvals 12,
  metrics 3); `POST /api/v1/reports` corrected to `implemented (Phase 6)`;
  13 live-but-stale Phase 7 rows (accounts index/lookup/search, suggestions ×4,
  announcements ×5, donation campaigns) flipped to `implemented (Phase 7)`;
  auth-paragraph and scope-registry notes updated.
- `docs/compatibility-profile.md`: Phase 7.3 surface marked implemented;
  new rows D35 (two-role split), D36 (approval modes), D37 (rules/warnings),
  D38 (deletion worker), D39 (metric keys/retention/space_usage), D40 (trend
  ordering/crawler/tag store), D41 (trend visibility).
