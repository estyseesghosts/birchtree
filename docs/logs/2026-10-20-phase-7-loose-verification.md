# 2026-10-20 — Phase 7 loose ends verification evidence

Executed on the dev machine (no ffmpeg: `which ffmpeg` empty; live-toolchain
tests gate on `validate_toolchain` and skip).

## `cargo check --all-targets`

Warning-free (`Finished dev profile`, no warnings). Fixed during
integration: domain one-param `Result` misuse in new push code, redb
borrow violations, missing struct-literal fields, Arc-vs-deref in one new
test file, two stale-clock `claim_due` calls, one invalid test scope.

## `cargo test` (full)

479 passed, 0 failed, 1 ignored (lib 138). New coverage:

- `tests/phase7_conversations_push.rs` (9): auth/scopes, list/read/unread/
  delete (+per-viewer, second-delete 404), thread grouping + cursors,
  direct-only privacy, mute/unmute flipping `muted`, push CRUD +
  validation, tickle delivery against loopback, `handle_push_job`
  gone-endpoint path, live-SSE `conversation` event on `direct`.
- `tests/phase7_web_odds.rs` (10): session+CSRF auth paths, embeds
  local/remote, settings sync, web push CRUD, link timeline, apps
  verify_credentials, relation reads, bundle 404.
- `tests/phase7_1_transcode.rs` (17): decision/passthrough matrix,
  startup validation (missing binary, missing encoder, bad knobs),
  EXIF fixture, v1-200/v2-202-206 lifecycle, crash/partial-output,
  `meta` rules. Fake-ffmpeg POSIX-sh harness; no network.
- `tests/phase7_2_search.rs` (8): three-mode parity, accounts path,
  hashtags on database path, edit/delete/visibility convergence,
  backfill resume + wipe-and-rebuild, non-indexable exclusion.
- `tests/phase7_policy_gaps.rs` (16): D22 three modes over REST home +
  stream gate, D23 filter/round-trip, D25 parity + backfill idempotency +
  tag-timeline unification, D26 all drop conditions + retroactive-leave.

Pre-existing suites: all green, including `phase5_streaming` (updated
`direct` expectation: exactly one `conversation` event) and
`phase4_search_emoji` (status hits via cold-index database fallback).

## `scripts/smoke-all.sh`

Green, including the extended `scripts/smoke-phase7.sh` (register two
users, DM → conversations list/read/mute/unmute/delete, push
subscribe/show/delete, `timelines/link`, `apps/verify_credentials`,
blocks/mutes/favourites/bookmarks/followed/featured reads, status
search hit + anonymous empty, v1 PNG upload 200 with URL, `/app` 404
unconfigured).

## Config behavior verified

- Default config validates (transcoding off in the hermetic default
  path); `--no-transcode` honored by smoke scripts and test server.
- `transcode_config()` with bogus ffmpeg path errors (unit test).
- Serve startup with transcoding on and no ffmpeg exits non-zero with
  `transcoding: ...` (manual run during integration).
