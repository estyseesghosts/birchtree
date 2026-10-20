# 2026-10-20 — Phase 7.3 integration (admin API)

## Context

`HANDOFF_admin-api.md`: implement Phase 7.3 (roles, full admin API, reports
queue, trends, annual reports) via four parallel subagents, then synthesize on
the shared pieces (role-gate shape, `REST::Admin::*` packers, audit trail),
wire the router, update the manifest, and run the gate.

## What the subagents delivered

- **A (roles + scope plumbing):** `UserRole` (`None`/`Moderator`/`Admin`,
  serde-default so pre-7.3 rows decode role-less with no migration),
  `set_user_role`, 16 `admin:*` scopes (no invented sub-scopes for
  trends/tags/measures/dimensions/retention), `Authenticated::require_admin`
  (`AdminArea::{Accounts, Reports, AdminOnly}`; order `require_user` 422 →
  role 403 `Forbidden` → scope 403 scopes-message), `ApiError::role_forbidden`,
  `create-account --role=` + `grant-role`, `tests/phase7_3_roles.rs` (13→16
  tests).
- **B (accounts + reports queue):** `application/admin.rs`,
  `persistence/redb/admin.rs` (+`ADMIN_AUDIT` table, v8 migration),
  `endpoints/admin_accounts.rs` (v1+v2 index, show, destroy, enable,
  unsensitive/unsilence/unsuspend, approve/reject, action) and
  `endpoints/admin_reports.rs` (index/show/update/assign/unassign/reopen/
  resolve), `tests/phase7_3_admin_accounts_reports.rs` (8 tests). Planned gaps
  P7.3-B1…B4 (approval modes, rules, deletion worker, filters/serializers).
- **C (infra rows):** domain allows/blocks, email-domain blocks, IP blocks
  (+`ip_block_middleware` + `is_sign_up_blocked_for` wired by the integrator),
  canonical email blocks (+`test` on the read scope), tags, measures/
  dimensions/retention on read scopes, `tests/phase7_3_admin_infra.rs`
  (11 tests). Planned gaps: federation/confirmation-gated enforcement,
  omitted metric keys.
- **D (trends + annual reports):** public trends (approved-only, offset/limit,
  `/trends` alias + `Deprecation: @1648612800`), 12 admin approval rows,
  annual reports with durable `GenerateAnnualReport` job, `tests/phase7_3_*`
  (10 + 3 tests). Planned gaps: link crawler, score ordering, tag follows.

## Integration changes (main session)

- Router: all B/C/D routes registered; `.merge(admin_infra::routes())`;
  `ip_block_middleware` layered inside the rate limiter; `POST /api/v1/accounts`
  gated by `is_sign_up_blocked_for` (403 `Forbidden`, same shape as closed
  registration).
- Audit convergence: one table (`ADMIN_AUDIT`, ascending keys); C's
  `admin_audit`/`admin_audit_log` moved onto it; dead `ADMIN_AUDIT_LOG`
  constant/table dropped.
- Removed A's temporary duplicate `GenerateAnnualReport` arm (D's real
  dispatch owns it).
- Cross-edits by parallel workers repaired: missing `ReadableTable` import,
  borrow in `ensure_trend_tag`, axum-`get` helper collisions in D's tests,
  `Uri`/`&Uri` and `&Arc<Store>` deref mismatches.

## Verification

- `cargo check --all-targets`: **0 warnings/errors** (gate requires
  warning-free).
- `cargo test`: **passed=401 failed=0** (baseline was 349/349; +52 new
  Phase 7.3 tests across 5 targets).
- HTTP smoke (`/tmp/smoke-73.sh`, live debug server + CLI role grants):
  **9/9** — 401/422/403-role/403-scope shapes, moderator split, public
  trends, reports queue, measures with read scope.
- Evidence: `docs/logs/2026-10-20-phase-7-3-verification.md` (includes the
  four upstream findings and five gate-run bug fixes).

## Contract updates

- `docs/endpoint-manifest.md`: 7.3 rows → `implemented (Phase 7.3)`;
  `POST /api/v1/reports` → `implemented (Phase 6)`; 13 stale Phase 7 rows
  → `implemented (Phase 7)`; auth + scope-registry prose updated.
- `docs/compatibility-profile.md`: Phase 7.3 marked implemented; D35–D41
  added (two-role split, approval modes, rules/warnings, deletion worker,
  metric keys, trend ordering/crawler/tag store, trend visibility D41).

## Open items (out of scope or follow-ups)

- Approval registration modes (Phase 2 gap) block real approve/reject.
- Deletion worker + federation Undo for account deletion.
- Link-preview crawler for trend cards; trend-score pipeline; tag follows.
- `--trends-enabled` CLI flag (config-only today); `trends_enabled=false`
  serving path is tested.
- Phase 7 client-facing groups (conversations, push, `/api/web/*`, etc.)
  remain `planned (Phase 7)` except the 13 rows verified live and flipped.
