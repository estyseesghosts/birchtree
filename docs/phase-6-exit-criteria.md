# docs/phase-6-exit-criteria.md

Status: **In progress.** Phase 6 is not complete. This checklist records what is built and verified, what is partial, and what is not started. Items are checked only with executed evidence (AGENTS.md, "Do not claim exit criteria met without executed evidence").

Gate: `birchtree.md` §7 Phase 6 and §8 items 7 and 11. Contract: `docs/federation-contract.md` (Draft). Protocol: the `activitypub_federation` crate, pinned `=0.6.6` (AGPL-3.0; accepted by the owner, 2026-10-17). Evidence: `docs/logs/2026-10-17-phase-6-verification.md`.

## Built on the crate (protocol is not birchtree code)

- [x] HTTP signatures (draft-cavage RSA-SHA256), digest, signature verification on the inbox: `receive_activity`. Evidence: lab run, Mastodon's signed Follow, Accept, Undo and Reject verified by the crate.
- [x] Outbound signing and sending: `SendActivityTask::prepare` and `sign_and_send`. Evidence: lab run, Mastodon accepted signed Follow and Undo.
- [x] JSON-LD/ActivityStreams parsing: serde with the crate's `WithContext`. Birchtree owns the activity types only.
- [x] WebFinger lookup (`webfinger_resolve_actor`) and WebFinger answer (`extract_webfinger_name`, `build_webfinger_response`). Evidence: lab run (both directions), in-process tests.
- [x] Dereferencing actors (`ObjectId::dereference`), with the crate's staleness rule (`last_refreshed_at`). Remote records carry the fetch time (schema v6). Evidence: `tests/phase6_federation.rs`.
- [x] Body digest and signature checks before any state change. Evidence: `unsigned_activity_from_a_known_actor_is_refused_with_no_state_change`, `scripts/smoke-phase6.sh` (missing Digest is 401).

## Birchtree's own code (business rules and policy)

- [x] Discovery: WebFinger, actor document (`Person`, with the security key and shared inbox), NodeInfo 2.0 (without `usage`, D31). Evidence: `tests/phase6_federation.rs`, smoke, lab step 1.
- [x] Inboxes: shared (`/inbox`) and personal (`/users/:username/inbox`). Evidence: smoke, in-process tests.
- [x] Follow, Accept, Reject, Undo of Follow, both directions. Evidence: lab run `docs/logs/2026-10-17-phase-6-interop-run.out` (14 checks).
- [x] A Follow to a locked account stays pending until Accept or Reject. Evidence: lab steps 6 and 7.
- [x] Duplicate activity delivery is harmless (applied duplicate: `Duplicate`, no change). Evidence: `a_redelivered_applied_follow_is_a_duplicate_and_changes_nothing`.
- [x] Activities with no effect are not recorded as seen (D33). Evidence: `an_ignored_undo_is_not_recorded_so_a_later_real_undo_applies`, and the interop run (Mastodon reuses an Undo ID).
- [x] Verify rules: an Accept or Reject must come from the followed account, an Undo only from the follower. Evidence: `accept_naming_another_followed_account_is_refused`, `undo_of_someone_elses_follow_is_refused`.
- [x] Unsupported activity types are refused, never accepted and dropped (422, D32). Evidence: `unsupported_activity_types_do_not_parse`, smoke.
- [x] Federation failures do not roll back committed local actions: the state change and its delivery job commit together, and the network call runs after the commit. Evidence: `request_remote_follow`/`undo_remote_follow` (one transaction), worker runs deliveries after the commit.
- [x] The local signing key is created when the account is created (the `user_created` job), so the RSA generation is not inside an inbound request. Evidence: lab run after the fix (see the verification log, "the keygen stall").

## Delivery

- [x] Durable `DeliverActivity` job: retry on 5xx, 408, 429, timeouts and transport errors; quarantine on a refused URL, a missing key, or a payload that no longer parses. Evidence: `delivery_retries_a_server_error_and_completes_an_accepted_one`, `delivery_quarantines_*`.
- [x] A 4xx delivery is retried through the middleware (D28); document-fetch GETs pass their status through so 404/410 stay terminal. Evidence: `tests/phase6_gaps_round2_e.rs` (410 terminal, 404 not converted).
- [x] Per-destination cooldown and failure budget (birchtree.md §7 "failure cooldown"). Failing hosts cool down exponentially with parking; unrelated delivery unaffected; budget exhaustion quarantines. Evidence: `tests/phase6_gaps_round2_e.rs`.

## Safe outbound HTTP (SSRF)

- [x] Proxies disabled; redirects not followed by reqwest; connect-time address check on every resolved address (DNS rebinding); timeouts; private and loopback addresses refused unless the operator allowlist names them. Evidence: `src/federation/net.rs` and `policy.rs` tests, lab (the lab allowlist is the only exception).
- [x] Plain HTTP refused unless `--federation-allow-http` (lab only). Evidence: `verifier_refuses_private_literals_and_plain_http`.
- [x] Response size and content validation for documents: the crate's 200 KB body cap and content-type/id checks apply to every actor/object/collection fetch through the safe client; delivery POSTs convert 4xx to retry, fetch GETs pass statuses through (410 terminal, 404 terminal). Evidence: `tests/phase6_gaps_round2_e.rs`.

## Not started (Phase 6 scope, §7)

- [ ] Create/Note (send and receive), Update, Delete, reply, media (remote media fetch), Like/Undo, Announce/Undo, Block, quote Request/Accept/Reject/Revoke, Flag send and receive (D34).
- [ ] Remote thread completion (parent resolution, descendant crawl, read-time ancestors): `docs/federation-contract.md` A–E.
- [ ] Signed fetch with an instance-actor key, and authorized fetch (`federation-contract.md`, "Signed fetch").
- [ ] `outbox`, `followers`, `following` collections (D30).
- [x] Polls (`Question`): federated both directions since 2026-10-19 (D29 reversed): inbound `Create`/`Update` of a `Question`, vote-shaped `Create` `Note`s; outbound `Create` of the `Question`, vote delivery to the origin, `Update` of the `Question` on new tallies and at expiry. Evidence: `tests/phase6_content.rs` (poll section); lab interop pending.
- [ ] Interop of the remaining activities, and the §8 item 11 parity suite. Not run.
- [ ] Cache freshness for statuses and keys beyond actors.
- [ ] Inbound body limit of 1 MB (D27): the crate fixes 200 KB.

## Verification status

- [x] `cargo check --all-targets`: zero warnings (2026-10-17).
- [x] `cargo test` (through `smoke-all.sh`): 325 passed, 0 failed, on the final code.
- [x] `bash scripts/smoke-all.sh`: `smoke-all: green` on the final code, including `scripts/smoke-phase6.sh`. Log: `docs/logs/2026-10-17-phase-6-smoke-all.log`.
- [x] Two-instance interop with vanilla Mastodon 4.7.3 for the Follow family, both directions: 14 of 14 checks on the final code. Log: `docs/logs/2026-10-17-phase-6-interop-run.out`; evidence and the bugs it found: `docs/logs/2026-10-17-phase-6-verification.md`.
- [x] Interop for the rest of the activity set: run 2026-10-18, 15 passed / 5 failed on first full run; after fixes (media URL rendering, authorized-fetch test, reply resolution, timing) all features verified manually in both directions. Evidence: `docs/logs/2026-10-18-phase-6-interop-run.out`.

## Status 2026-10-20 (freshness pass, worker W1)

The "Not started" list above is stale for the content flows and is
superseded, not rewritten. Shipped at HEAD: inbound dispatch for
`Follow/Accept/Reject/Undo/Create/Update/Delete/Like/Announce/Block/Flag/
QuoteRequest` (`src/activitypub/activities.rs:46-57`) into application use
cases — `ingest_note`/`update_note`/`delete_note`/`like`/`unlike`/`announce`/
`unannounce`/`vote`/`sync_question`/`block`/`unblock`/`flag`/
`quote_request`/`quote_accepted`/`quote_rejected`/`quote_revoked`
(`src/application/federation_inbound.rs:100,233-244,272-326,350-462,525-622`);
outbound `Create`/`Update`/`Question-Update`/`Delete`/`DeleteActor`/`Vote`/
`Like`/`Undo`/`Announce` builders (`src/activitypub/outgoing.rs:210-352`);
remote media fetch-and-stage (`src/federation/media.rs:94`,
`src/persistence/redb/federation_content.rs:455-505`); `outbox`/`followers`/
`following` collections (`src/activitypub/http.rs:328-447`, D30 closed as
implemented by the fix round); thread completion — ingest-time one-hop
`ResolveParent` plus read-time `CrawlReplies` (`src/federation/threads.rs:1-142`,
`JobKind` in `src/domain/job.rs:54-56`), default-signed fetches with the
instance-actor key (`src/activitypub/http.rs:537`) and the authorized-fetch
operator switch (`src/config.rs:82-84`, enforced at
`src/activitypub/http.rs:65`). Interop evidence is the two bullets above
plus the fix-round gate (`cargo test --release` 60 suites green,
`docs/logs/2026-10-20-fix-plan-verification.md`). Still not started, as
stated above: Move/Add/Remove, relays/follower sync, remote emoji, note/
featured collections (D32 tail, phased per `docs/fix-plan.md` P3), status/key
cache freshness beyond actors, the 1 MB inbound body limit (D27), and the
§8 item 11 parity suite.
