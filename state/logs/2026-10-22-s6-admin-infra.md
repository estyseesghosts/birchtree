# 2026-10-22 — S6 admin infra vertical (decomposition proposal)

## What changed
Endpoint layer (`endpoints/admin_infra.rs`, 1880 lines → dir, shim file
deleted):
- `admin_infra/mod.rs` (gates, `routes()`, shared helpers, explicit
  re-exports preserving `ip_block_middleware` / `is_sign_up_blocked_for` /
  `REFUSED_*` paths), `domain_allow/block`, `email_domain`, `ip_block`,
  `canonical_email`, `tags`, `measures` (orchestrator 23 lines),
  `dimensions` (23), `retention` (22). Largest file 561 lines (mod).
Persistence layer (`redb/admin_infra.rs`, 1125 lines → 7 files, shim
deleted):
- `admin_infra_{domain,email,ip,tags,metrics}.rs` (verbatim bodies as
  `pub(super)` free fns), `admin_infra_audit.rs` (audit + 7 rule fns as
  free fns + `audit_append` wired into `admin_audit`), `admin_scan_ctx.rs`
  (moved `page_desc`/`get_by_desc`/3 scan caps), `admin_infra_impl.rs`
  (new: the SINGLE `impl AdminInfraStore for RedbStore` — 52 one-line
  delegations; Rust forbids splitting one trait impl across files).
- File sizes: audit 195, impl ~300, domain ~200, email ~215, ip ~140,
  tags ~165, metrics ~310 — all <400.
- `AdminPage` adoption deferred (signatures unchanged; S12 sweep
  material). `bounded_scan`/`write_audit_txn` helpers NOT landed
  (would-be-new dead code; call sites keep verbatim loops — a follow-up
  may convert them one by one). `require_admin_action` (new, uncalled)
  deleted rather than kept as dead code.
- Endpoints reference sibling types via `super::` (mod.rs owns shared
  items); `redb` files import only what they use (`ReadableTable` added
  where the compiler demanded it).

## Verification
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test` (full, with S4/S7a/S8/S11): 1153 passed, 0 failed.
- `rustfmt --edition 2024 --check` on all touched files: clean.
