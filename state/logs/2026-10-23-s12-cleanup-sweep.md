# 2026-10-23 — S12 cleanup sweep (decomposition proposal)

## What changed
Mechanical cleanup per proposal S12 bullets 1–8, no behavior change:

- Bullet 1 (timelines): `tag_filtered` (8 params) → `TagFilterQuery` struct;
  `HomeFilter`/`PublicFilter`/`TagFilter`/`ListFilter` + mention predicates
  extracted into `application/timelines/filters.rs`. Endpoint caller migrated.
- Bullet 2 (search): `search` (10) + `search_with_mode` (12) adopt S0
  `SearchQuery` (`&SearchQuery` + mode/store). Endpoint + `phase7_2_search`
  callers migrated.
- Bullet 3 (notifications): `evaluate_policy` (6) + `render_notifications`
  (6, endpoint) → `NotificationCtx { store, recipient_id, now_ms }`.
  Streaming `render.rs` caller migrated. `index` split into
  parse/load stages; `render_notifications` split into batch/row stages.
- Bullet 4 (collections): `items_add` (6), `items_index` (7),
  `scan_notifications` (6) → `CollectionCtx` + `CollectionScan`.
  Axum adapters construct the context; `gaps_collections` helper migrated.
- Bullet 5 (statuses endpoint): `create` (~175) → auth+parse+delegate
  (`parse_create_command`, `schedule_response`, `created_response`);
  orchestrator now 49 lines delegating to `application/statuses/create_status`.
  S5 leftovers also cut over: `create_status`/`update_status` slimmed onto
  `create_stages`/`update_stages`; redb `plan_poll`/`apply_patch_txn`/
  `enqueue_create_jobs_txn`/`statuses_by_account_impl` slimmed onto
  `statuses_*_stages`.
- Bullet 6 (outgoing): `note_for_status` (6), `question_for_status` (7),
  `quote_request` (8), plus `create`/`update`/`question_update` →
  `OutgoingCtx { store, public_url, now_ms, license_policy, place_policy }`.
  `note_inner` split into mention/attachment/location/quote stages.
  `events_build`, `objects`, `http` (incl. outbox), and federation tests
  migrated. `threads_auth::authorize_step` (136) split onto
  `threads_auth_stages`.
- Bullet 7 (media): `build_argv` (6→5, config-owned ffmpeg path),
  `run_transcode_plan` (7→ctx), `commit_derived` (8→ctx) via `TranscodeCtx`;
  `stored`/`metadata_only` (6–7) via `CacheEntryCtx`; `walk_sorted`,
  `rollback_settle` allows deleted (at threshold). `transcode.rs` (1823)
  split into `transcode/` (10 leaves + mod with tests); `activities.rs`
  (1451) into `activities/` (6 leaves); `endpoints/accounts.rs` (1755)
  into `accounts/` (9 leaves); `endpoints/statuses.rs` (1229) gains
  `statuses/interactions.rs`; `remote_cache.rs` (1211) gains
  `remote_cache/reservation.rs`. S6/D splits: suggestions `rank_suggestions`,
  reports `create`, notifications `index`/`render`, accounts `register` +
  directory `search`, webauthn `verify`, CLI `parse_args`, measures/
  dimensions `render_keys`, retention `render_cohorts` all <100.
- Bullet 8 (lint ceiling): all remaining
  `#[allow(clippy::too_many_arguments)]` carry `// WHY:` comments
  (9 sites); 3 stale allows deleted. Single-txn bodies grandfathered with
  `// WHY single-txn:` (accounts, admin, polls, relationships,
  remote_cache ×2, statuses_interactions).
- Hygiene (session): `Cargo.toml` dev/test profiles set
  `debug="line-tables-only"`, `incremental=false` (target/ disk budget);
  `.gitignore` adds `/.cargo/`.

## Verification
- `cargo check --all-targets`: exit 0. Warnings are pre-existing only —
  `admin_infra_impl.rs` braces ×7, `adv2_b_cache` dead `small_bytes`,
  `fix_emoji_cache`/`adv2_b_reconcile`/`fix_delivery_order` test imports.
  Zero warnings in S12-touched files (fixed one self-introduced
  `remote_cache.rs` unused `Mutex`/`OnceLock` import; fixed `pid`/`config`
  warnings in transcode split).
- `rustfmt --edition 2024`: new S12 leaf files formatted (write mode on
  leaves only, never `mod.rs`); modified-file pre-existing drift left alone.
- Full `cargo test`: 1158 passed, 0 failed (1153 baseline + 5 S10 snapshot).
- Done-definition audit: no `src/**/*.rs` ≥1200 lines; no unjustified
  `too_many_arguments` allows; grandfathered single-txn bodies listed above.
- `scripts/smoke-all.sh` NOT run (needs bash; WSL shim — native gates used).
