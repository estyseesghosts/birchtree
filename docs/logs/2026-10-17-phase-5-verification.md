# 2026-10-17 — Phase 5 verification (streaming)

Append-only evidence for `docs/phase-5-exit-criteria.md`. The work log is `state/logs/2026-10-17-phase-5-streaming.md`. Starting point: master at `2202253` (Phase 4.1), tree clean. The Phase 5 changes are one commit, `feat(phase-5): …`, made after this log's runs. Nothing is pushed.

## 1. Gate: `bash scripts/smoke-all.sh`

Run on the Phase 5 tree. Full output captured to a file and reduced to these lines (ANSI colour removed):

```
==> cargo check --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.86s
==> cargo test
… (per-binary lines, below)
==> scripts/smoke-phase1.sh
SMOKE_OK
==> scripts/smoke-phase2.sh
SMOKE_PHASE2_OK
==> scripts/smoke-phase3.sh
SMOKE_PHASE3_OK
==> scripts/smoke-phase4.sh
SMOKE_PHASE4_OK
==> scripts/smoke-phase5.sh
SMOKE_PHASE5_OK
smoke-all: green
GATE_EXIT=0
```

- `cargo check --all-targets`: zero `warning:` lines in the full run.
- `cargo test`, per binary (`test result` lines, reduced):

```
src/lib.rs (unit)                 85 passed
crash_recovery                     3 passed
persistence_conformance           18 passed
phase2_api                        13 passed
phase2_oauth                      13 passed
phase3_interactions                8 passed
phase3_relationships              14 passed
phase3_status                     13 passed
phase4_1_exclusive_home            5 passed
phase4_1_home_cap                  4 passed, 1 ignored (the dense-follow measurement)
phase4_1_json_parity               6 passed
phase4_filters                    15 passed
phase4_lists                       5 passed
phase4_media                       8 passed
phase4_notifications               4 passed
phase4_scheduled                   3 passed
phase4_search_emoji                3 passed
phase4_timelines                  23 passed
phase5_streaming                  31 passed
runtime                            3 passed
secret_handling                    3 passed
```

Total: **280 passed, 0 failed, 1 ignored** (Phase 4.1 gate: 240 passed, 1 ignored). The difference is the 31 protocol tests and the 9 new unit tests (`streaming::channels`, `streaming::auth`).

## 2. Phase 5 test inventory

`tests/phase5_streaming.rs` (31; real sockets through the real router):

```
a_client_that_never_reads_is_disconnected_at_the_queue_bound
a_policy_change_drops_frames_queued_before_it
a_stalled_transport_is_closed_after_the_stall_timeout
an_unanswered_websocket_ping_closes_the_connection
block_retracts_home_statuses_and_silences_both_directions
concurrent_follow_unfollow_block_and_delete_never_leak_after_commit
connection_caps_refuse_with_the_documented_status
direct_and_remote_public_streams_authorize_and_stay_quiet
edit_and_delete_reach_the_public_and_home_streams
event_queue_overflow_closes_every_stream
health_is_open_and_opens_with_a_comment
heartbeat_is_sent_on_an_idle_stream
list_stream_carries_members_and_honors_replies_policy
local_media_and_hashtag_streams_select_their_statuses
media_stream_carries_only_statuses_with_media
mentioned_viewers_get_edits_on_their_notification_stream
mute_retracts_and_hides_on_public_streams
notification_stream_delivers_favourites_on_both_user_streams
parameter_refusals_match_upstream_text
public_stream_carries_public_statuses_and_self_replies_only
public_stream_hides_statuses_that_mention_a_muted_account
revoking_a_token_closes_its_streams
sse_headers_match_upstream
sse_refusals_carry_upstream_status_and_text
streaming_path_redirects_to_a_configured_host
subscriptions_per_websocket_are_capped
total_connection_cap_refuses_with_503
user_stream_carries_followed_and_own_statuses_until_unfollow
websocket_errors_come_back_as_error_frames
websocket_protocol_subscribes_by_query_and_by_message
websocket_refuses_the_upgrade_with_an_error_header
```

Unit tests (9): `streaming::channels` (`sse_paths_map_to_upstream_channels`, `ws_names_include_the_media_and_locality_variants`, `notification_stream_needs_its_own_scope_family`, `hashtag_and_list_require_their_parameter`, `hashtag_is_normalized_and_keys_match_upstream_channels`, `user_stream_folds_notifications_only_with_the_scope`, `websocket_frame_stream_names_carry_the_parameter`) and `streaming::auth` (`header_wins_then_query_then_protocol`, `scopes_need_read_or_the_stream_family`).

## 3. Mutation checks

Each mutation was applied to a backed-up file, the named test was run, and the file was restored and compared byte-for-byte (`cmp`).

| # | Mutation | Test run | Result |
|---|---|---|---|
| M1 | Remove the policy-epoch drop in `Outbox::next` | `a_policy_change_drops_frames_queued_before_it` | FAILED (`0 passed; 1 failed`); restored |
| M2 | Remove the mention block/mute check from `public_stream_includes` (the first attempt) | `block_retracts_home_statuses_and_silences_both_directions` | passed: the author gate still held, so this run did not exercise the mention check. Gap found; test added below |
| M2b | Same mutation, after adding `public_stream_hides_statuses_that_mention_a_muted_account` | that test | FAILED; restored |
| M3 | Disable the retraction (`fanout::retract` returns early) | `mute_retracts_and_hides_on_public_streams` | FAILED; restored |

## 4. Live smoke (`scripts/smoke-phase5.sh`)

Against the live binary (`--registration-open`, `--public-url` on an ephemeral port). Steps and their assertions:

```
==> refusals and the health probe           (health OK; 401 Missing; 400 Unknown channel; 400 Missing tag)
==> public stream: create, edit, delete     (:) opening; update; status.update; delete with the id)
==> home stream: follow, deliver, block retracts, block stays silent
==> notification stream: a favourite notifies the author's stream   (job worker running; event notification, type favourite)
==> WebSocket: upgrade refusal header, then the public frame        (HTTP/1.1 401; X-Error-Message; frame stream ["public"])
==> revocation closes an open stream        (POST /oauth/revoke 200; the curl stream process exits)
SMOKE_PHASE5_OK
```

The smoke uses `curl -N` for SSE and a stdlib-only WebSocket client (`python3 -I`). It is a scripted protocol client, not an unmodified Mastodon client.

## 5. Parity (`birchtree.md` §8 item 11): not run

- **Not run:** no unmodified client, and no live Mastodon v4.7.3 instance. The harness is not built (as in Phase 4.1).
- **Host facts, checked 2026-10-17:** `command -v ruby bundle psql redis-server` returned nothing. The Docker daemon answers (`docker info`, client 29.9.0). Local images: `postgres:16-alpine`, `postgres:15-alpine`, `redis:7-alpine`, `valkey/valkey:9.1.2-alpine`, `nginx:alpine`, and two federation images. No Mastodon image. A pinned instance could be built from the tag with Docker, which needs network access for gems and npm. Not attempted here (escalation E1).
- **Pinned source:** the upstream checkout `~/fedifedifedi/mastodon` is on `main` at `7f90e0fd72`, ahead of the pin. The tag `v4.7.3` resolves to `8ff11c2b`. Every upstream read in this phase used `git show 8ff11c2b:<path>`, never the working tree.
- **Observations:** none live. The streaming behaviour birchtree reproduces is source-derived from `8ff11c2b`: the channel names and parameters, the scope families, the refusal texts, the SSE and WebSocket envelopes, the `:)` opening and `:thump` heartbeat, the WebSocket ping, the host redirect, the anonymous public payload with a logged-in `filtered` array, the delete payload (the id), the notifications-channel fold for `user`, the retraction rule (`clear_from_home`, `clear_from_lists`), and the deliberately absent events (unfollow, domain block, suspension).
- **Surfaces covered by no live observation:** every Phase 5 stream and event. Phases 0–4.1 remain as their own logs describe (also not run live).

## 6. Measurements

No performance measurement was taken. Test wall times for context: `phase5_streaming` ran in about 4.6 s; the whole `cargo test` run is in §1.

## 7. Flakiness check

The protocol suite is timing-based (see the exit criteria, "residuals"). Three further consecutive runs were made after the gate; §8 records them.

## 8. Flakiness runs

Three consecutive runs of `cargo test --test phase5_streaming` after the gate:

```
test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.56s
test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.57s
test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.61s
```

Together with the runs during development (each suite run on every change), no intermittent failure was seen. Three runs do not prove absence of flakiness under load; the windows in the no-leak test are the first place to look if one appears.
