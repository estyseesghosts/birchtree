# 2026-10-20 — Phase 7.3 trends + annual reports (Subagent D)

## What changed (all additive; no edits to router.rs, scopes.rs, auth.rs, endpoint-manifest.md, main.rs)

New domain (`src/domain/trends.rs`, `src/domain/annual_report.rs`):
- `TrendTagFlag` / `TrendStatusFlag` / `TrendLink` / `TrendPublisher` (approval flags + link/publisher candidate rows; card metadata `None` until the crawler lands), `TagUsage`, `AnnualReport` + `NewAnnualReport` (`ANNUAL_REPORT_SCHEMA_VERSION = 1`, birchtree's own data shape).
- `JobKind::GenerateAnnualReport` (`generate_annual_report`).

Config (`src/config.rs`, `src/api/mastodon/mod.rs`): `trends_enabled` (default true, mirrors upstream `Setting.trends`; no CLI flag — main session call).

Ports (`src/application/ports.rs`): `TrendStore` + `AnnualReportStore` added to the `Store` bound. Audit reuses subagent C's `AdminAuditRecord` table; trend reviews write the audit row in the same txn. Added `trend_audit_trail` read for verification. NOTE for synthesis: C's `AdminTag.trendable` and my `TrendTagFlag.allowed` model the same upstream `Tag#trendable` column — unify into one store (wire shapes already match: both serialize `trendable`).

Persistence (`schema.rs` v8, `migrations.rs` v8 block, new `persistence/redb/trends.rs`):
- Tables `trend_tag_flags`, `trend_status_flags`, `trend_links(+_by_url)`, `trend_publishers(+_by_domain)`, `annual_reports(+_by_account)`, `admin_audit_by_target` (shares C's `admin_audit` table). Reused C's `ADMIN_AUDIT` const (removed my duplicate).
- Reviews commit flag + audit atomically. Candidates materialize idempotently on read. `account_statuses_in_year` walks the author index oldest-first with a year-bound early stop (ids embed ms), capped.

Application (`application/trends.rs`, `application/annual_reports.rs`):
- Offset/limit parsing (upstream `limit_param` semantics: default 10 tags/links, 20 statuses; max 2×default), approved-only public derivations, `extract_urls`, viewer gating for statuses (`show_status` + mute/block/domain-block exclusion = `filtered_for`), locale noted as ordering-only/no-op (no per-tag locale data).
- Annual stats from real records (statuses/favourites/reblogs counts, top-5 statuses, top-10 hashtags); honest zeros for empty years; `eligible`/`generating`/`available`/`ineligible` states.

Worker (`jobs/handlers.rs`): `GenerateAnnualReport` arm runs the idempotent generation use case.

DTOs (`dto.rs` append): `trend_tag_json`, `admin_trend_tag_json` (+`trendable`/`requires_review`), `trend_link_json` (null card fields until crawler), `admin_trend_link_json` (+`id`), `admin_trend_publisher_json`, `annual_report_json` (`share_url: null` — sharing out of profile), `annual_reports_json`.

Endpoints (main session wires into `router.rs`; snippet in final return):
- `endpoints/trends.rs`: `trends_alias` (GET /api/v1/trends + `Deprecation: @1648612800`, verified 2022-03-30 = 1648612800), `tags_index`, `links_index`, `statuses_index`. OptionalAuth, no scope, offset Link headers (next when full, prev when offset > limit).
- `endpoints/annual_reports.rs`: index/show/state (read|read:accounts), generate (202/200{}), mark-read (write|write:accounts); `:id` is the year, unparseable → 404.
- `endpoints/admin_trends.rs` (flat module, matching B/C convention): tags/links/statuses index + approve/reject, publishers index (cursor paging, default 100/max 200) + approve/reject. Gate: `require_admin(AdminArea::AdminOnly, admin:read|admin:write)` — role 403 `Forbidden` vs scope 403 vs app-only 422.

Tests: `tests/phase7_3_trends.rs` (9 tests), `tests/phase7_3_annual_reports.rs` (3 tests). Mount the handlers on a test router sharing one `ApiState` (router.rs frozen). Admin tokens: HTTP registration (admin scopes now in registry) + `set_user_role`; scoped-down tokens minted directly via the store.

## Verification status

SKIPPED per relayed instruction (no `cargo check`/`cargo test` — shared target-dir lock; main session verifies at the end). Statically reviewed instead. Known build risk: teammates' in-flight edits (`StatusMetricRow` unresolved import in ports.rs at last check) break the whole tree independently of this work; my additions may need a rebase pass once the tree is green. My two suspected borrow issues (closure captures, entry API) were rewritten defensively by hand.

## Planned gaps (for manifest/deviation register)

- Link preview crawling: approved cards serve `null` metadata until a safe-client crawler lands (never fabricated).
- Trends ordering: tags by real usage count desc; links/statuses newest-first (upstream: trend score). No score pipeline.
- Locale: accepted but ordering-only/no-op (no per-tag locale data; statuses carry language but client-locale negotiation unimplemented).
- Tag `history`: `[]` (no per-day usage history yet); tag `following`/`featuring`: `false` (no tag-follow store — Phase 7).
- No `--trends-enabled` CLI flag (config default on).
- `POST /api/v1/reports` manifest row: router line 216 already implements it (`reports::create`); flip manifest line 239 from `planned (Phase 7.3)` to `implemented (Phase 6)`.

## Open synthesis items for main session

- Wire the router snippet (below) into `router.rs`; flip manifest rows for trends/admin-trends/annual-reports to `implemented (Phase 7.3)` (I was told not to touch the manifest).
- Unify `TrendTagFlag.allowed` with `AdminTag.trendable`; add `AdminInfraStore`/`AdminStore` to the `Store` bound when their impls land (mine are already in).
- Confirm 403 `Forbidden` for role-less (matches upstream Rack status text per subagent A).
