# 2026-10-22 — S10 DTO + status view + streaming (decomposition proposal)

## What changed
Layering violation fixed first: `filter_json`, `filter_result_json`,
`filter_v1_json` (+ shared `rfc3339_ms`) moved verbatim to the new
`application/ports/filter_json.rs` (wire shapes in the application layer);
`api::mastodon::dto` re-exports them; `application/filters.rs` calls the
ports path. `grep 'use crate::api' src/application/` returns zero.

Contract gate: `tests/s10_dto_snapshot.rs` (5 tests) written BEFORE any
split — fixed clock/ids, pins `status_json` (Content+Source, anon+viewer),
`account_json`/`account_json_with_stats`, poll rendering, and all three
filter serializers byte-for-byte. Green before, green after.

`dto.rs` (1444 lines) → `dto/` (mod + 7 leaves, all <500 lines):
`account.rs` (serializers + shared `tag_url`), `status.rs` (`StatusView`,
`QuoteView`, `status_json` split into `pack_base`/`pack_viewer_flags`/
`pack_quote`, `source`/`edit`), `notification.rs` (known missing
`filtered`/`fallback`/`report`/`event` keys documented as gaps, nothing
added), `poll.rs`, `media.rs`, `admin.rs`, `instance.rs`. `mod.rs` keeps the
small Phase 4 serializers (`list/marker/scheduled/emoji/trend-link/search`)
verbatim and re-exports every pre-existing `dto::X` path (grep-verified
callers across endpoints/streaming/tests unchanged).

`status_view.rs` (656 → 469 lines): `StatusViewContext` now holds one
`StatusViewBatch` (`status_view_batch.rs`, new, 314 lines) instead of 13
hand-rolled caches; `prefetch` is one `load_for_statuses` call; all
accessors read `batch.*` with the same point-lookup fallbacks. Loader table
verified query-for-query against the old `prefetch` (same tables, bounds,
sort/dedup, viewer gates). `MAX_PREFETCH_ROUNDS` is the single round bound
(shared `pub(crate)`); `EventHead`-style deviation: batch takes
`load`/`load_for_statuses`; `quote_probed` stays context-local.

`streaming/fanout.rs::status_frames` (7 params → `&Core, &FrameCtx` where
`FrameCtx { now, sub, status, edited, event }`; the per-event `anonymous`
cache stays with the caller). `Hub::new` intentionally NOT touched: it is
4 params, already under the lint bar (struct would churn 20+ call sites for
no gain).

## Verification (per function group, then whole)
- Layering: `cargo check` exit 0 + zero `use crate::api` in `application/`.
- Snapshot: 5/5 before the split, 5/5 after.
- View/streaming: `phase5_streaming` (31) + `phase3_status` (13) green.
- `cargo check --all-targets`: exit 0, zero new warnings (14 pre-existing
  locations only, none in touched files; one pre-existing `rustfmt` import
  drift in `fanout.rs`/`threads.rs` left alone per no-churn rule).
- `rustfmt --edition 2024 --check`: clean on all new/touched S10 files.
- Full `cargo test`: 1158 passed, 0 failed (1153 baseline + 5 snapshot).
- `scripts/smoke-all.sh` NOT run (needs bash; WSL shim — native gates used).
