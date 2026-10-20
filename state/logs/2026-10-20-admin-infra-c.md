# 2026-10-20 — Phase 7.3 subagent C: admin infra rows

## What changed (no cargo run — verification deferred to the main implementer)

New modules (all additive; no edits to router.rs, scopes.rs, auth.rs,
endpoint-manifest.md, main.rs):

- `src/domain/admin_infra.rs` — `AdminDomainAllow`, `AdminDomainBlock` (+
  `AdminDomainSeverity`), `AdminEmailDomainBlock`, `AdminIpBlock` (+
  `AdminIpSeverity`), `AdminCanonicalEmailBlock`, `AdminTag`, `AdminAuditRecord`,
  plus metric projections (`AdminAccountOverview`, `AdminReportTarget`,
  `AdminMediaOwner`). Reuses subagent A's `UserRole`; no role constants of my
  own. (Also: `#[serde(default)] role` already existed on `LocalUser` from A —
  untouched.)
- `src/application/admin_infra.rs` — pure use cases: domain normalize/validate
  (pinned `Validation failed` messages), canonical-email normalize/hash/validate
  (upstream `CanonicalEmail`), CIDR parse/contain/CIDR-text, IP severity parse,
  `no_access`/`sign_up_block` matchers, trusted-proxy `client_ip`. Unit tests
  inline.
- `src/application/ports.rs` — `AdminInfraStore` trait (+ `AdminDomainBlockPatch`,
  `AdminIpBlockPatch`, `AdminTagPatch`, `StatusMetricRow`) and `Store`
  supertrait/blanks extended. Plus `admin_audit_log` read for tests.
- `src/application/mod.rs` — `pub mod admin_infra;`.
- `src/domain/mod.rs` — module + re-exports.
- `src/persistence/redb/schema.rs` — 13 tables, `SCHEMA_VERSION` 7 → 8.
- `src/persistence/redb/migrations.rs` — step v8 (tables only, no backfill);
  import list extended.
- `src/persistence/redb/admin_infra.rs` — `impl AdminInfraStore for RedbStore`
  (one txn per method; `desc_id`-keyed newest-first pages with exclusive
  max/since/min cursors; bounded scans with `note_scanned`).
- `src/persistence/redb/mod.rs` — `pub mod admin_infra;`.
- `src/api/mastodon/endpoints/admin_infra.rs` — handlers + `require_admin`
  wrapper (pinned `AdminArea::AdminOnly`, easy rename), DTO packers, `routes()`
  fragment, `request_client_ip`, `ip_block_middleware` (reads
  `ConnectInfo<SocketAddr>` like `main.rs` serves, falls back to raw
  `SocketAddr`), `is_sign_up_blocked_for`. Measures/dimensions/retention with
  real computation only; uncomputable keys omitted (upstream `filter_map`
  semantics).
- `src/api/mastodon/endpoints/mod.rs` — `pub mod admin_infra;`.
- `tests/phase7_3_admin_infra.rs` — 10 fixture-backed tests (own test router
  mounting `admin_infra::routes()`; tokens minted via the store with admin
  scopes).

## Reused from subagent A (already in tree)

- `Authenticated::require_admin(store, AdminArea, scopes)` + `AdminArea::AdminOnly`
  (role-before-scope, 403 `Forbidden` for role failure), `ApiError::role_forbidden`,
  `UserRole`, `set_user_role`, `admin:*` scope registry, `Account::{silenced,
  sensitized}`.

## Planned gaps (store-only until the dependency lands; also in the report)

- Domain blocks: no federation enforcement (Phase 6).
- Email-domain + canonical email blocks: no registration enforcement (needs
  email confirmation, Phase 2 gap).
- Retention activity = authored a status, not sign-ins (no login tracking;
  documented deviation for the manifest).
- Omitted metric keys: `active_users`, `interactions` (redis ActivityTracker),
  `resolved_reports` (no resolution state), `tag_*` (no tag trend tables),
  `instance_follows`/`instance_followers`, dimensions `software_versions`,
  `tag_servers`, `tag_languages`, `instance_accounts`, `instance_languages`;
  `space_usage` covers media bytes only.
- Audit: my `admin_audit_log` table vs B's `admin_audit` table — both hold
  `AdminAuditRecord`; synthesis should converge on one (B's comment says
  `ADMIN_AUDIT` is "shared with `AdminInfraStore::admin_audit`").

## Verification status

- SKIPPED: relay instruction forbade `cargo check/test/build` (shared target-dir
  lock). Code was review-passed only (borrow discipline, import hygiene,
  signature parity against the ports trait). The main implementer must run
  `cargo check --all-targets`, `cargo test`, and
  `cargo test --test phase7_3_admin_infra` at merge time.
- Pre-existing breakage observed (not mine, left for owners): B's
  `endpoints/admin_accounts.rs` (`&Arc<dyn Store>` vs `&dyn Store`), D's
  `application/trends.rs` (private imports) — these fail `cargo check` on the
  current tree independently of this work.
