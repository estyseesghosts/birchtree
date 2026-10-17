# 2026-10-17 — Phase 5: streaming (SSE and WebSocket)

Follows `state/logs/2026-10-17-phase-4-1-parity-gap-closure.md`, which stays unchanged. Evidence: `docs/logs/2026-10-17-phase-5-verification.md`. Gate: `docs/phase-5-exit-criteria.md`. Contract: `docs/streaming-contract.md` (Draft).

## Read first (as briefed)

`AGENTS.md`; `birchtree.md` §1, §2.6, §3, §5, §7 Phase 5, §8 items 8 and 11; `docs/streaming-contract.md`; `docs/application-ports.md`; `docs/architecture.md`; `docs/testing.md`; `docs/endpoint-manifest.md` (streaming rows); `docs/compatibility-profile.md`; `docs/phase-4-1-exit-criteria.md`; the Phase 4.1 state and verification logs.

Upstream was read at the pin: `git -C ~/fedifedifedi/mastodon show 8ff11c2b:<path>`. The checkout's working tree is `main` at `7f90e0fd72`, ahead of the tag, so the working tree was not used for any upstream claim. Files read at the pin: `streaming/index.js` (all 1478 lines), `streaming/errors.js`, `streaming/utils.js`, `streaming/database.js` (head), `app/controllers/api/v1/streaming_controller.rb`, `config/initializers/1_hosts.rb`, `app/lib/feed_manager.rb`, `app/services/fan_out_on_write_service.rb`, `remove_status_service.rb`, `batched_remove_status_service.rb`, `notify_service.rb`, `app/workers/push_update_worker.rb`, and the callers of the feed-clearing functions.

## Outcome

| Item | Result |
|---|---|
| SSE and WebSocket server, in-process | implemented (`src/streaming/`) |
| Declared streams | `user`, `user:notification`, `public` family, `hashtag` family, `list` implemented; `direct` and `public:remote` accepted and silent (D18, D19) |
| Event envelopes | `update`, `status.update`, `delete`, `notification` (SSE and WebSocket shapes as upstream) |
| Gates | the REST timeline policy (`application::timelines`), plus upstream's streaming check on mentions for public and hashtag streams |
| Live policy | block and mute retract; block, mute, unfollow, remove-follower and domain block drop in-flight frames (policy epochs) |
| Bounds | caps, queues, stall timeout, heartbeat, ping, fail-closed overflow (D14, D15) |
| Host redirect | `--streaming-url`, `301` to a different host (D16) |
| Tests | `tests/phase5_streaming.rs` (31), 9 unit tests; mutation-checked (3) |
| Smoke | `scripts/smoke-phase5.sh` (`SMOKE_PHASE5_OK`), added to `smoke-all.sh` |
| Gate | `smoke-all: green`; `cargo check --all-targets` zero warnings; 280 passed, 0 failed, 1 ignored |
| §8 item 11 parity | **not run** for Phase 5 (escalation E1) |
| Exit | not claimed as met: the parity item is open and client smoke coverage is partial (`docs/phase-5-exit-criteria.md`) |

## What changed

Code:
- `src/streaming/` (new contents): `mod.rs` (StreamError), `channels.rs` (names, parameters, channel ids, keys), `auth.rs` (token precedence, resolution, scopes, list ownership), `hub.rs` (registry, queues, caps, stall sweep, policy epochs, `StreamingSink` adapter, `Session` guard), `fanout.rs` (one committed event against every subscription), `render.rs` (REST DTO payloads), `http.rs` (endpoint dispatch, redirect, health), `sse.rs`, `websocket.rs`.
- `src/application/streaming.rs` (new): `StreamEvent`.
- `src/application/timelines.rs`: stream gates `home_includes`, `public_stream_includes`, `tag_stream_includes`, `list_stream_includes`; `retraction_candidates`.
- `src/application/ports.rs`: `StreamingSink` port, `NoStreaming`, `DeletedStatus`; `StatusStore::delete_status` returns `DeletedStatus`; `StatusStore::own_reblog`.
- `src/persistence/redb/statuses.rs`: the cascade returns the wrappers' snapshots; `own_reblog` (`REBLOGS_BY_ACCOUNT`).
- `src/application/context.rs`: `Context` carries `streaming`. `Context::new` takes the sink (5 arguments).
- `src/api/mastodon/mod.rs`: `ApiState` carries `streams: Hub`; `ServerInfo` gains `streaming_host` and a configured `streaming_url`; `StreamingHost` (parse, same origin, redirect location).
- `src/config.rs`, `src/main.rs`: `streaming_api_base_url` (`--streaming-url=`), validated; hub built and wired to the context, the state and the job worker.
- `src/api/mastodon/router.rs`: `GET /api/v1/streaming` and `GET /api/v1/streaming/{*any}` route to the streaming endpoint.
- Publication points (after the use case returns `Ok`): `endpoints/statuses.rs` (create, edit, delete, reblog, unreblog), `endpoints/relationships.rs` (block, mute, unfollow, remove follower, domain block), `api/oauth/token.rs` (revoke → `TokenRevoked`).
- Job path: `jobs/handlers.rs` and `jobs/worker.rs` take the sink; `FanOutNotification` and `PublishScheduledStatus` publish once, after their commit. `application/notifications.rs::fan_out_from_payload` and `application/scheduled_statuses.rs::publish_scheduled_job` return the created id.
- `endpoints/notifications.rs`: `render_notifications` takes its dependencies, so streams share the REST rendering.

Tests and harness:
- `tests/phase5_streaming.rs` (new, 31 tests).
- `tests/common/mod.rs`: `TestServer` carries `streams` and `with_limits`; `tests/runtime.rs` and the job-handler call sites in `tests/persistence_conformance.rs`, `tests/phase4_media.rs`, `tests/phase4_notifications.rs`, `tests/phase4_scheduled.rs` pass the sink.
- `scripts/smoke-phase5.sh` (new); `scripts/smoke-all.sh` runs it.
- `Cargo.toml`: unchanged (`axum` `ws`, `tokio-tungstenite` and `futures-util` were already present).

Docs (all in place, nothing earlier edited): `docs/streaming-contract.md` (rewritten from the stub, still Draft), `docs/phase-5-exit-criteria.md` (new), `docs/logs/2026-10-17-phase-5-verification.md` (new), this log (new), `docs/endpoint-manifest.md` (streaming rows to implemented; Phase 5 honesty list; envelope note), `docs/compatibility-profile.md` (Phase 5 surface; rows D14–D26), `docs/application-ports.md` (Phase 5 status), `docs/testing.md` (Phase 5 suites; gate line), `docs/README.md` (index), wiki (`Roadmap`, `Feature-Matrix`, `Current-State`, `Home`).

Not changed, as required: `AGENTS.md`, `docs/phase-4-exit-criteria.md`, `docs/phase-4-1-exit-criteria.md`, every earlier `docs/logs/` and `state/logs/` file.

## Decisions I took (reversible; please confirm or redirect)

1. **Publication is in the adapter, after the use case returns.** Use cases are free functions over `&dyn Store`; threading a sink through each would have touched every caller. The job handler publishes after its own commit. A future federation inbound path must call the sink too; this is a rule to carry forward.
2. **Streams are best-effort, at most once, not persisted.** Matches upstream's Redis pub/sub. Overflow closes every stream rather than dropping an event silently (fail closed).
3. **Gates reuse the REST policy.** `HomeFilter`, `PublicFilter`, `TagFilter`, `ListFilter` and `ViewerScope` are unchanged; new public functions expose one status at a time. Streams add only upstream's mention check for public and hashtag streams (the streaming check), which the REST public timeline does not make.
4. **Live policy has two mechanisms.** Gates are re-evaluated per delivery, so future events obey current state. Policy epochs drop frames queued before a visibility-removing change at the transport. Retraction (block and mute only, as upstream) is computed before the commit, because a block unfollows and changes the feed's authors.
5. **Retraction is capped at 200** (D15). Upstream removes every feed entry; a cap bounds the pre-commit scan.
6. **Limits** (D14) are my choice: 512 connections, 16 per token, 16 subscriptions per WebSocket, 256 frames or 1 MiB per connection, a 30 s stall timeout, a 4096-event queue, and 15 s / 30 s heartbeat and ping. Recommended, not decided; the operator may change them (`streaming::Limits`).
7. **Stalled client policy** is the bounded one: a queue that is full, or a transport with queued frames and no progress for 30 s, closes the connection. Nothing is buffered past the bound. This is the trade the brief asked me to escalate if it traded memory for delivery; it does not, so I did not escalate it, and I say so here.
8. **Same-origin streaming is served, not `404`'d** (D16). The Rails controller's `404` exists because nginx routes streaming paths to Node; birchtree is one process.
9. **`Vary: Authorization` stays on streaming responses** (D17). Removing it would need a route exemption in the global header layer; no client reads it.
10. **Hashtag normalization is the REST normalizer** (`normalize_tag`), so streams and the tag timeline agree (D25). Upstream's `normalizeHashtag` differs for accented and compatibility-form tags. Changing it is a REST change; escalated.
11. **The WebSocket `?stream=` subscription is decided before the upgrade response**, so an event committed right after connecting is not missed. Upstream subscribes after the upgrade. The observable frames are the same.
12. **`delete_status` returns the cascade.** The store already tombstones the reblogs; returning their snapshots is exact and bounded by the store's existing `CASCADE_SCAN_CAP`.
13. **Repeat reblogs publish nothing** (the store returns the existing wrapper; the handler checks first), as upstream's `ReblogService` returns early.
14. **Upstream's `filters_changed` is not transmitted** (its payload is absent, so upstream drops it). birchtree publishes nothing for filter changes; `filtered` is computed at delivery.

## Verification

See `docs/logs/2026-10-17-phase-5-verification.md` for pasted outputs.

- `bash scripts/smoke-all.sh`: `smoke-all: green`, exit 0 (SMOKE_OK, SMOKE_PHASE2–5_OK).
- `cargo check --all-targets`: zero warnings.
- `cargo test`: 280 passed, 0 failed, 1 ignored (the Phase 4.1 measurement).
- Mutation checks: the epoch drop, the retraction and the mention gate each fail their test when disabled. One mutation survived the first time (the mention gate, with no test covering it); a test was added and it now fails.
- The protocol suite ran four times after the last change, all 31 passing.
- Live smoke: create, edit and delete over `curl -N`; follow, delivery, block retraction and silence; notification from the job worker; WebSocket frame shape and refusal header; revocation closing an SSE and a WebSocket stream.

## Open items and escalations

- **E1. Parity harness (`birchtree.md` §8 item 11).** Not run for Phase 5. The host has no Ruby, Postgres or Redis, but Docker is available with Postgres and Redis images, so a pinned v4.7.3 could be built from the tag. Operator decision: build it now (network needed) or defer and keep the gap in the evidence. The brief asked for this decision early; I did not make it.
- **E2. Home policy gaps** (D22 blocks and mutes on mentions; D23 follow languages). Both are Phase 4 gaps in `application::timelines::HomeFilter`, which REST and streams share. Upstream's `FeedManager#filter_from_home` applies both. Fixing them changes REST home. Not changed here.
- **E3. Notifications from blocked and muted accounts** (D26). `NotifyService` drops them upstream; birchtree creates and streams them. A fix belongs to notification fan-out (Phase 3/4).
- **E4. Hashtag normalization** (D25). Changing `normalize_tag` changes the REST tag timeline.
- **E5. Bounds and the retraction cap** (D14, D15) are recommended values, not decided.
- **E6. AGENTS.md** (not edited, per the brief). Its Phase 4 paragraph should say 4.1 is implemented. Its "Current phase" line should say Phase 5 is implemented, pending the exit decision and E1.
- **Residuals** (in the exit criteria): the no-leak test is windowed in time (its assertion is on commit order); the fan-out is one thread with an O(subscriptions) gate cost per event (untested under load; Phase 8).
- **Not built** (by phase): `conversation` and announcement events (Phase 7), remote public streams (Phase 6), suspension kill (Phase 7.3), chosen-languages filter (Phase 2 settings).
- **Commit:** made as `feat(phase-5): …` on `master`, not pushed.
