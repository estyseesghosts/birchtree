# 2026-10-22 — S7b federation orchestrator rewires (decomposition proposal)

Four orchestrators rewired to their S7a stage modules, one function at a
time, each gated before the next. All moves verbatim; no semantic change.

## What changed
- `threads::check_authorization_remote` (106 → 14 lines): delegates to
  `threads_auth::QuoteAuthCtx::new(...).authorize_step()`. Verified
  arm-by-arm against the original (404/410 revoke, policy-refused keep,
  `fetches` vs `fetches+1` chain accounting, all strings). Call sites
  unchanged. Follow-up dedup in same slice: `revoke_verified` single-homed
  to `pub(crate)` in `threads_auth.rs`, `threads.rs` copy deleted.
- `delivery::run` (216 → 20 lines): `parse_delivery_job`,
  `park_if_sequence_blocked`, `park_if_cooling`, `load_sender_keys`,
  `send_and_settle` in `delivery_send.rs`. Private `delivery::classify`
  deleted (single home `delivery_send::classify`); inline settlement now
  goes through `charge_budget`, inline parks through `enqueue_deferred`
  (byte-identical outcomes verified, incl. store-failure `Retry` mapping).
  Constants referenced from `delivery`, never redefined.
- `events::run` (373 → 71 lines): `EventHead` + `load_head` (job prelude),
  five builders (already landed), new `enqueue_relays` +
  `enqueue_deliveries` tails in `events_build.rs`; five private helpers
  promoted to `pub(crate)` in `events_build.rs`, `events.rs` copies
  deleted. `plan_inbox_forward`/`run_relay_forward` untouched. Deviation
  from S7a note: tails moved out too (needed to reach <80; relay budget
  semantics unchanged).
- `media::fetch` (339 → 70 lines): `reserve_room`,
  `head_status_and_cap`, `stage_streamed`, `stage_buffered` in
  `media_fetch.rs`; `commit_bytes` still delegates to `settle_staged`
  (100-line body never copied). Superseded `get_signed` + `lock_ledger`
  deleted (no callers left; signed-fetch covered by `signed_get`, proven by
  `a_signed_media_fetch`/`c_media_http`). `ledger_outcome` → `pub(crate)`.
  `Stream` classification, reservation shrink, all reason strings identical.

## Verification (per-function, before next)
- threads: `phase6_gaps_round2_e` + `gaps_federation_tail` + `phase6_content`
  (62 tests) green.
- delivery: `phase6_federation` + `phase6_gaps_round2_e` +
  `fix_delivery_order` + `adv2_c_ordering` + `gaps_federation_tail`
  (59 tests) green.
- events: `phase6_content` + `phase6_federation` + `phase6_gaps_round2_e`
  + `gaps_federation_tail` (80 tests) green.
- fetch: `adv2_a_fetch` + `adv2_d_stream` + `fix_media_class` +
  `fix_media_http_stream` + `a_signed_media_fetch` + `c_media_http` +
  `adv2_a_settle` (38 tests) green.
- `cargo check --all-targets`: exit 0, zero new warnings (14 pre-existing
  locations only, none in `federation/`).
- `rustfmt --edition 2024 --check`: clean on all seven touched federation
  files (one pre-existing import-block drift in `threads.rs` left alone).
- Full `cargo test`: 1153 passed, 0 failed — matches pre-slice baseline.
- `scripts/smoke-all.sh` NOT run (needs bash; WSL shim — native gates used).
