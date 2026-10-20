# Phase 5 — exit criteria checklist

Source: `birchtree.md` §7 Phase 5 ("Streaming") and §8 items 8 and 11. Contract: `docs/streaming-contract.md` (Draft). Evidence: `docs/logs/2026-10-17-phase-5-verification.md`. Work log: `state/logs/2026-10-17-phase-5-streaming.md`. Register rows: D14–D26 (`docs/compatibility-profile.md`).

**Exit (§7):** protocol tests plus client smoke tests cover each declared stream; no cross-user or private leakage under concurrent follow, block, unfollow and status-delete changes; stalled clients cannot exhaust memory or tasks.

**Result: not claimed as met.** Every item below is checked against executed evidence. Items marked partial or open say why.

## Implementation (§7 Phase 5 bullets)

- [x] SSE and WebSocket hosted in-process (`src/streaming/`); no separate streaming service.
- [x] Event envelopes: `update`, `status.update`, `delete`, `notification`. Each carries the REST DTO, or the id for `delete`.
- [x] Streams: `user`, `user:notification`, `public` and its `local`, `remote` and `media` variants, `hashtag` and `hashtag:local`, `list`. `direct` and `public:remote` are accepted and held open, and are silent until Phases 7 and 6 (D18, D19).
- [x] Token types and scopes, stream parameters (`tag`, `list`, `only_media`), filtering, blocks, mutes, domain blocks and status visibility, through the shared gates in `application::timelines` (no streaming-only policy).
- [x] Host discovery and redirect: `urls.streaming` derived from the public URL or set by `--streaming-url`; a `301` to a different configured host (D16).
- [x] Heartbeats (SSE `:thump` every 15 s; WebSocket ping every 30 s, closed without a pong), connection and subscription caps, cleanup on every exit path (the session guard), disconnect and reconnect, and backpressure (bounded per-connection queue, stall sweep, fail-closed overflow).
- [x] Publication after commit through `StreamingSink`; no publication inside a storage transaction.
- [x] Live policy: block, mute, unfollow, remove-follower and domain block change what open streams carry (policy epochs; retraction on block and mute, as upstream).

## Exit criteria

- [x] **Protocol tests cover each declared stream.** `tests/phase5_streaming.rs` (31 tests, real sockets through the real router): every stream family above, with its gate and its events.
- [~] **Client smoke tests cover each declared stream.** Partial. `scripts/smoke-phase5.sh` runs against the live binary and covers `public`, `user` (delivery and block retraction), `user:notification` (a job-created notification) and a WebSocket stream (frame shape and refusal header). `hashtag`, `list`, `public:media` and `direct` are covered by protocol tests only. None of these is an unmodified Mastodon client: no such client runs on this machine (keystodon is not present), so the smoke tests are scripted protocol clients.
- [x] **No cross-user or private leakage under concurrent changes.** `concurrent_follow_unfollow_block_and_delete_never_leak_after_commit` (a storm of posts and deletes against follow and unfollow churn, then a block, then ten posts: none reaches the viewer's home or public stream). `a_policy_change_drops_frames_queued_before_it` (the in-flight frame is dropped). Mutation checks: disabling the epoch drop, the retraction, or the mention gate each fails its test (verification log §3).
- [x] **Stalled clients cannot exhaust memory or tasks.** `a_client_that_never_reads_is_disconnected_at_the_queue_bound`, `a_stalled_transport_is_closed_after_the_stall_timeout`, `event_queue_overflow_closes_every_stream`, the caps (`connection_caps_refuse_with_the_documented_status`, `total_connection_cap_refuses_with_503`, `subscriptions_per_websocket_are_capped`).
- [x] **Heartbeat and liveness.** `heartbeat_is_sent_on_an_idle_stream`, `an_unanswered_websocket_ping_closes_the_connection`.
- [x] **Auth and scopes.** `sse_refusals_carry_upstream_status_and_text`, `parameter_refusals_match_upstream_text`, `websocket_refuses_the_upgrade_with_an_error_header`, and the unit tests in `src/streaming/auth.rs`.
- [x] **Gate: `cargo check --all-targets` with zero warnings** (verification log §1).
- [x] **Gate: `cargo test` passes** (verification log §1).
- [x] **Gate: `bash scripts/smoke-all.sh` ends `smoke-all: green`** (verification log §1; the run's last line is the proof).
- [ ] **§8 item 11, the unmodified-client parity suite, is not run for Phase 5.** No live Mastodon v4.7.3 instance runs here (no Ruby, Postgres or Redis on the host). Docker is installed and the Postgres and Redis images are present locally, so a pinned instance could be built from the tag; that is an operator decision (escalation E1). Every streaming observation so far is source-derived from `8ff11c2b`, and it is protocol-tested against birchtree only. Surfaces covered by no live observation: all of Phase 5. Surfaces not covered by a test of any kind: none of the declared streams.
- [x] **Every deliberate difference is in the register** (D14–D17, D25, D26 are new; D18–D24 are gaps with their phases).
- [ ] **Operator approval for `Frozen`.** Pending. `docs/streaming-contract.md` stays `Draft`.

## Known residuals (not register rows)

- The no-leak race test uses time windows: positive checks wait up to 5 s, and negative checks watch 0.5–0.8 s. A violation that lands after the window would not be seen. The assertion is on commit order, not timing, so a seen violation is always real.
- The fan-out is one thread. Each event costs one gate evaluation per subscription (a few store reads each). Measured only at the test scale, not under load (Phase 8).
- Upstream normalizes hashtags with `normalizeHashtag`; birchtree uses its REST normalizer (D25). The gap predates Phase 5 and also affects the REST tag timeline.
- A WebSocket subscribes to the `?stream=` query before its upgrade response, where upstream subscribes after it. The observable result is the same; the first frame after upgrade is a refusal in both.

## Escalations (decisions this phase does not make)

- **E1. Parity harness.** Build a live pinned v4.7.3 instance now (Docker, from the tag, network needed), or defer §8 item 11 and record the gap in the evidence. The brief asked for this decision early; it is open.
- **E2. Home and streams: blocks and mutes on mentions, and follow languages** (D22, D23). The shared Phase 4 home policy differs from `FeedManager#filter_from_home`. A fix changes REST home as well as streams.
- **E3. Notifications from blocked and muted accounts** (D26). Upstream drops them; birchtree creates them. A fix is a Phase 3/4 change to notification fan-out.
- **E4. Hashtag normalization** (D25). Changing `normalize_tag` alters the REST tag timeline.
- **E5. Bounds and the retraction cap** (D14, D15). The values are birchtree's choice (recommended, not decided). The operator may set them.
- **E6. AGENTS.md** (not edited, per the brief). Its Phase 4 paragraph should say Phase 4.1 is implemented; its Phase 5 line should say Phase 5 is implemented, pending the exit decision.

## Status 2026-10-20 (freshness pass, worker W1)

E6 is resolved without touching this file's history: `AGENTS.md:19-26` now
carries the status line ("fix round complete (2026-10-20), Phase 8 next",
phases listed incl. 4.1/5/6-partial/7/7.1/7.2/7.3). E2's shared-policy area
was reworked by the 2026-10-20 fix round (viewer-side mute/domain gates
`src/application/policy.rs:21-80`, folded into `can_fetch_full` at
`:120-123`; strictness deltas registered D45/D46/D47 per `docs/fix-plan.md`
P0) — a later worker should re-check D22/D23 against the
new gates rather than assuming this file's E2 text. E1 (parity harness) is
still open, consistent with `birchtree.md` §8 item 11's 2026-10-20 note.
E3–E5 remain as stated.
