# 2026-10-19: configurable status limit + bench run4

## Product change: `--max-status-chars=` (default 500, ceiling 3000)

Operator request (run4 handoff): birchtree's 500-char limit becomes
configurable; "our max" is 3000 (Misskey parity). Single enforcement point
was `create_status()` (`application/statuses.rs:237`); no config file
exists (CLI flags only).

- `config.rs`: `max_status_chars` (default 500), validate rejects 0 and
  >3000, 3 new unit tests.
- `main.rs`: `--max-status-chars=` flag (1..=3000, else exit 2).
- `ServerInfo.max_status_chars` from config; v1 (+ new `&ServerInfo` param)
  and v2 instance `max_characters` advertise it.
- `create_status()` takes `max_chars`; over-long message interpolates the
  limit (identical to Mastodon's at default). `DEFAULT_MAX_STATUS_LENGTH`,
  `ABS_MAX_STATUS_LENGTH` public consts.
- `schedule_status()` validates length at schedule time (same message;
  previously accepted, then silently dropped at publish). Limit travels in
  row params; `publish_scheduled_job` replays with the stored limit.
- `README.md` flags table; `docs/compatibility-profile.md` open decision
  "Maximum status/media limits" now decided for statuses.
- Tests: `tests/phase3_status_limit.rs` (4); `cargo check --all-targets`
  clean (one pre-existing warning); full `cargo test --release` 347
  passed / 0 failed. Release binary `ee6bfe94122a12c7`.

## Bench run4 (builds pt `f58be005`, bt `ee6bfe94`)

Full report: `/root/pinetree/docs/federation-bench-2026-10-19-phase7-run4.md`;
live log: `...-run4-log.md`; problems 51-53 appended. Headlines:
renote/boost 0 FAIL, media-into-bt 0 FAIL, akkoma->bt polls fixed;
new root-caused (not fixed) product bug: DELETE of a reblog sends
`Delete`, never `Undo(Announce)` (item 52, Mastodon-parity, proven live
vs real Mastodon 4.7.3) — explains 9 unboost rows. Fix sketch in item 52.

## Open questions

- Item 52 fix implementation + regression test (next session).
- Unclassified carry-overs: S2 pair misses, S3 failed vote calls,
  akkoma-outbound family, catodon tallying, S6 attribution, interop
  follow-visibility, 422/403 senders.
