# 2026-10-19 — item-52 reblog-delete Undo fix (run5 pending)

## Changed (birchtree only, uncommitted)
- `src/persistence/redb/statuses.rs` `delete_status`: when the deleted status
  has `reblog_of_id`, additionally enqueue `undo_announce` with
  `status_id = target` and `announce_uri = wrapper uri`, and drop the
  `REBLOGS_BY_ACCOUNT` mapping. The `delete` event is kept for followers'
  timelines. Mastodon parity; pinetree reference is `delete_note`
  `was_announce_of_uri` branch (`src/application/commands/notes.rs:325-343`,
  `src/federation/outbox.rs:517-524`).
- `tests/phase6_content.rs`: two regression tests —
  `a_reblog_delete_enqueues_undo_announce`,
  `an_ordinary_delete_enqueues_no_undo_announce`.

## Verified
- New tests pass; full `cargo test --release`: 349 passed / 0 failed
  (run4 baseline 347 + 2 new).
- Binaries rebuilt; disk shas: birchtree `e24d75aea1085a06`
  (post-fix build), pinetree `f58be005c558f650` (unchanged).
  Run IDs to be taken from `/proc/<pid>/exe` at `bench-up`.
- `df -h /root`: 855G avail before build.

## Open
- `bench-up` to `/root/bench/run5` in progress at time of writing; full
  `run-suites.sh` + Phase 7 run5 report + problems-54 appendix follow after
  the run. Expected (not guaranteed): 9 bt-booster unboost rows flip;
  akkoma/misskey/catodon rows stay red per item 53.
- Nothing committed in either repo.
