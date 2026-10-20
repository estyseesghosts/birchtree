# 2026-10-17 — Phase 6: ActivityPub federation (in progress)

Follows `state/logs/2026-10-17-phase-5-streaming.md`, which stays unchanged. Gate: `docs/phase-6-exit-criteria.md`. Evidence: `docs/logs/2026-10-17-phase-6-verification.md`. Contract: `docs/federation-contract.md` (Draft).

## Step 0 decisions

Each decision names who made it and what is still open.

| # | Decision | Made by | Status |
|---|---|---|---|
| E1 | Build the pinned Mastodon v4.7.3 from tag `8ff11c2b` with Docker. Source is exported with `git archive` into `/root/parity/mastodon-v4.7.3` (outside the repo). The reference checkout `~/fedifedifedi/mastodon` is read with `git show` only and is not touched. Image: `birchtree-parity/mastodon:v4.7.3`. | The brief ("Build it from tag v4.7.3 ... with Docker"), read as operator approval. | Approved by the brief. Build log: `/root/parity/build-mastodon.log`. |
| E1a | Lab topology: a private Docker bridge `birchtree-lab` (172.28.0.0/24). Mastodon at 172.28.0.10, birchtree on the host at the bridge gateway 172.28.0.1. Two `/etc/hosts` lines map `mastodon.lab` and `birchtree.lab`. Lab-only HTTP. Mastodon gets `ALLOWED_PRIVATE_ADDRESSES=172.28.0.0/24`, its own documented knob for this. Only the Mastodon web port is published, on 127.0.0.1. The host's 5432 and 6379 are used by other containers and are not used. | Decided here. | Operator to confirm the `/etc/hosts` edit and the lab-only HTTP flag. |
| P1 | Crate pairing: the brief's stable `activitypub_federation` 0.6.6 (Axum 0.7.5 line). Checked against the source (`/root/parity/crate-review/`, read only). | The brief. | Recorded here; the crate is **not added** yet (see L1). |
| P1a | Pairing finding. In 0.6.6 the Axum inbox (`receive_activity`) is Axum-0.7 only, and the HTTP-signature functions are `pub(crate)`. Birchtree is on Axum 0.8. The crate's default URL verifier checks an address with one lookup and then connects by hostname, so it does not cover DNS rebinding. Its fetch and send use `FederationConfig.client`, which is public, so a safe client can be injected. | Decided here, from the source. | For the operator (see Open questions). |
| P1b | Until the operator decides P1a and L1, birchtree builds its own RFC-draft-Cavage RSA-SHA256 signing, digest handling and the safe HTTP client. They live in `src/activitypub/signature.rs` and `src/federation/net.rs`, behind narrow functions, so the crate can replace them later without touching callers. The brief says "Use the crate behind a narrow federation adapter", so this is a deviation from the plan and needs approval. | Decided here. | Open. |
| L1 | License. The crate is AGPL-3.0 (checked in its manifest). The brief requires operator confirmation before the crate is added. Birchtree has no LICENSE file in the repo. | The brief. | **Open: operator decision. The crate is not added.** |
| C1 | Proposed caps kept as proposed: 40 parent hops per chain, 1000 fetches per triggering request, and the crawl caps in `federation-contract.md` §D. | The brief ("keep these unless the operator changes them"). | Recorded. |
| Q1 | `Question` (polls). `FEDERATION.md` at 8ff11c2b documents `Question` and a 500-option limit, so polls are part of the pinned federation surface. Phase 6's activity set (`birchtree.md` §7) does not list them. Decision: not federated in Phase 6. Inbound `Question` and `Update` of a `Question` are refused explicitly (4xx with a logged reason), never accepted and dropped. Polls federate in a later phase, and the deviation goes in the register. | Decided here. | Recorded; deviation row to add. |
| S1 | Signature scope. The pinned FEDERATION.md says inbound RFC 9421 (`rsa-v1_5-sha256`) and Ed25519 are accepted by Mastodon 4.7, with draft-Cavage RSA-SHA256 as the baseline and outbound. Phase 6 implements draft-Cavage RSA-SHA256 inbound and outbound. RFC 9421 and Ed25519 are refused explicitly until implemented. | Decided here. | Recorded. |
| S2 | Inbound body limit 1 MiB (upstream: "Serialized JSON-LD 1MB, Activity is rejected/dropped"). The crate uses 200 KB; birchtree follows upstream. | Decided here. | Recorded. |

## Open questions for the operator

1. **L1, license.** May the AGPL-3.0 `activitypub_federation` crate be added? Birchtree has no declared licence. The crate's AGPL reaches a network service that is run for other people. This decides whether the crate is used at all.
2. **P1a/P1b, crate use.** Use the crate for the inbox and transport (requires either Axum 0.7 for the federation router or a mixed-version service adapter, and an injected safe client), or keep birchtree's own signature and HTTP layer? The recommendation is below.
3. **E1a, lab.** Approve the two `/etc/hosts` lines and the lab-only flags (`--federation-allow-http`, `--federation-allow-private=…`). Both are off by default and are required for any two-instance test.
4. **Q1, polls.** Confirm the deferral of `Question` federation.


## Session 2 (2026-10-17): rebuilt on the crate

Supersedes the open items above by reference. The entries above stay as they were written.

### Owner decisions in this session

- **L1 (license): resolved.** The owner accepted AGPL-3.0 for `activitypub_federation` ("the project is going to be open source, we are going to comply with the license"). Recorded in the handoff; the crate is a dependency now.
- **P1 (pairing): resolved.** Crate pinned to `=0.6.6` (`default-features = false`, feature `axum`), which needs axum 0.7. Birchtree moved from axum 0.8 to 0.7.9 (route syntax `:id` and `*any`, `#[async_trait]` on custom extractors, WebSocket `Ping(Vec<u8>)`).
- **P1a/P1b (crate use): resolved in favour of the crate.** Birchtree's own signature, digest, parser, inbox, fetch and send code is deleted. The owner's instruction: "take out all of your own federation code. we are building around the crate." Later instruction: rely on the crate for as much as possible, federate with vanilla Mastodon and prove it with results, and log inbound and outbound federation messages.
- **Transport:** birchtree's client (`federation/net.rs`) is injected into the crate: no proxies, the resolver refuses a name whose answers include a blocked address, reqwest does not follow redirects (the crate follows one, and its URL verifier checks it), connect and total timeouts. `federation/policy.rs` is the verifier's rule set.
- **Lab-only switches (off by default):** `--federation-allow-private=CIDR` (also turns on the crate's debug mode, which its production checks would otherwise refuse, since they refuse private addresses and need a domain), `--federation-allow-http`, `--federation-extra-ca=PEM` (a private CA for the lab; the system trust store is not changed).

### What was removed

Deleted (protocol code the crate replaces): `activitypub/signature.rs`, `parser.rs`, `inbox.rs`, `resolve.rs`, `outbound.rs`, `objects.rs`, `http.rs`, and the baseline placeholders `serializer.rs`, `signatures.rs`, `outbox.rs`, `webfinger.rs`. `application/crypto.rs` is back to baseline (its key generator is the crate's now). `application/federation_ports.rs` lost `FederationHttp`, `FetchError`, `Signer` and the fetch and post types. `tests/phase6_federation.rs` (the old one) was rewritten. The shared files the previous attempt had wired (`router.rs`, `relationships.rs`, `search.rs`, `main.rs`, `worker.rs`, `handlers.rs`) were restored to baseline and wired again, against the crate.

Kept and adapted: the store (`persistence/redb/federation.rs`, schema v6, migration v6 with an actor-URI backfill), the use cases (`application/federation.rs`), the domain keys type (`domain/federation.rs`, with `refreshed_at_ms`), the transport and policy modules, and the `DeliverActivity` job plumbing.

### What was built

- `activitypub/actor.rs`: `ActorRecord`, the crate's `Object` and `Actor` for local and remote accounts; the `Person` document; the local key is generated on a blocking thread.
- `activitypub/activities.rs`: `Activity` (untagged: Follow, Accept, Reject, Undo of Follow), with `ActivityHandler` on each. Outbound and inbound use the same types; the body is `WithContext<Activity>`.
- `activitypub/http.rs`: WebFinger, NodeInfo, the actor document, the shared and personal inboxes. `receive_activity` does the crate's inbound pipeline.
- `activitypub/outbound.rs`, `resolve.rs`: REST follow, unfollow and search `resolve=true`.
- `federation/delivery.rs`: the `DeliverActivity` job rebuilds the activity from its stored body, then `SendActivityTask::prepare` and `sign_and_send`.
- Store additions: `InboundEffect::{Reject, UndoFollow}`, `apply_inbound_reject`, `refreshed_at_ms` on remote keys, the actor-URI index for local accounts (written at creation, backfilled by migration v6).

### Bugs found by the interop run, and their fixes

1. **Undo ID reuse.** Mastodon reuses an Undo's ID (`…#follows/N/undo`) for a later undo of the same follow row. An Undo with no effect was recorded as seen, so the real one was reported as a duplicate and dropped. Fix: an `Ignored` outcome rolls back its transaction, dedup record included (D33). Regression test: `an_ignored_undo_is_not_recorded_so_a_later_real_undo_applies`.
2. **Accept verify checked the wrong field.** It compared the embedded follow's actor (birchtree's follower) with the sender. The correct check is the embedded follow's object (the followed account) against the sender. Mastodon's 403 would otherwise have left the follow pending forever, since Mastodon does not retry a 4xx.
3. **Local host and port.** Accounts use `host_from_url` (no port); the crate's domain keeps the port, because its `is_local` check compares `host:port`. Local lookups now use the account's domain, and the crate domain stays host:port (smoke on a non-default port).
4. **The keygen stall.** The local RSA key was generated on the first request that needed it. In a debug build that takes seconds; the inbound Follow then outlived Mastodon's 10-second timeout, which retried it. Fix: the `user_created` job creates the key (`jobs/worker.rs`, `ensure_local_keys_for_account`), in a detached task, because awaiting it in the worker held up the media job (the phase 4 smoke polls for 5 s). The lazy path remains as a fallback. Measured: inbound Undo handled in 1.5 ms (was 4.7 s), Follow in 12 ms (was ~17 s).

### Verification

- `cargo check --all-targets`: zero warnings (after each change).
- `cargo test`: 290 existing tests passed after the axum 0.7 migration, once two tests that compared unsorted key lists were sorted (the crate enables serde_json's `preserve_order`, which changes map order). Then 16 in `tests/phase6_federation.rs` and 3 new `activitypub` unit tests. Final count in the verification log.
- `scripts/smoke-all.sh`: `smoke-all: green` (see the verification log for the run used).
- Interop with vanilla Mastodon 4.7.3: `scripts/lab/phase6-interop.sh`; results in `docs/logs/2026-10-17-phase-6-interop-run.out` and `docs/logs/2026-10-17-phase-6-verification.md`.

### Open questions for the operator

1. **D27 (inbound body limit):** the crate fixes 200 KB; Mastodon accepts 1 MB. Accept the crate's limit, or add an extractor of our own at 1 MB?
2. **D28 (4xx is final):** the crate treats any 4xx except 408 and 429 as delivered; Mastodon retries. Accept, or wrap the send to see the status and retry?
3. **D29 (polls):** deferred for Phase 6 (decided). Confirm.
4. **Phase 6 scope:** the rest of the §7 activity set (Create/Note, Update, Delete, Like, Announce, Block, quotes, Flag, remote media) and thread completion are not started. They are the bulk of the rest of Phase 6. Do you want them in this phase before the exit, or split into a 6.1?
5. **Debug mode in the lab only:** the lab relies on the crate's debug mode (because of private addresses). Acceptable for a lab-only switch, with the warning at startup?

## Session 3 (2026-10-18): Phase 6 to completion

Owner decisions (2026-10-18):
- **Lab taken down** (all `bt-*` containers, `birchtree-lab` network, birchtree process, `/etc/hosts` restored from `/root/parity/lab/hosts.before-phase6`). Rebuild with `scripts/lab/README.md` when interop is next needed.
- **D27 closed:** inbox limit is 1 MiB. Done by a vendored copy of `activitypub_federation` 0.6.6 under `vendor/` with one constant changed (`[patch.crates-io]` in `Cargo.toml`). Reason: `ActivityData` has no public constructor and the signature/digest checks are `pub(crate)`, so an extractor alone cannot reach them. Remote document fetches stay at 200 KB.
- **D28 closed:** any 4xx is retried. Done by `federation::net::FailClientErrors`, a `reqwest-middleware` layer: a 4xx becomes a middleware error, the crate passes it through, and the job settles as a retry.
- **D29 stays deferred** (polls).
- **Phase 6 is not complete until** notes, renotes (Announce), quotes, blocks, flags, remote media, updates, deletes, likes, announces (both directions), and thread completion are all done. The lab may use the crate's debug mode. Production must never use debug.

Checklist (tick in this log as each lands):
- [x] D27 inbox 1 MiB (vendor patch)
- [x] D28 4xx retry (middleware)
- [x] Schema v7: status-by-URI index (backfilled), refresh table, reports, remote media source, quote-by-approval, unresolved parent; FederationEvent, ResolveParent, CrawlReplies, FetchRemoteMedia job kinds
- [x] Store: remote Note ingest (`NewStatus.remote`), update/delete by author, reply linking, unresolved-parent record, quote states (pending for remote quoted authors), federation events enqueued in the same transaction as each local change
- [x] Inbound handlers (`activitypub/activities.rs`): Create, Update (Note and Person), Delete (Note, Tombstone, actor, QuoteAuthorization), Like, Undo Like, Announce, Undo Announce, Block, Undo Block, Flag, QuoteRequest, Accept/Reject of QuoteRequest and of Follow
- [x] GET endpoints: Note (410 for a tombstone, 404 unless public or unlisted), QuoteAuthorization, followers and following (exact counts), outbox (20 newest public Creates), `/actor` (instance actor), actor document with collections
- [x] Outbound: `federation/events.rs` builds Create, Update, Delete, Like, Undo Like, Announce, Undo Announce, Block, Undo Block, QuoteRequest, Accept/Reject of QuoteRequest, Delete of QuoteAuthorization, Flag; one DeliverActivity per inbox
- [x] Remote media: `federation/media.rs` (URL policy, 40 MiB cap, sniffing, staged through the existing ProcessMedia job); outbound Documents use the public media URL
- [x] Thread completion: `federation/threads.rs` (ResolveParent with hop cap 40; CrawlReplies with caps 500/page, 500 pages, 1000 total; read-time trigger with the 5-minute wait and 15-minute cooldown, and the Mastodon-Async-Refresh header; ancestor completion on the same trigger)
- [x] Signed fetch: instance actor (`activitypub/instance.rs`, reserved key id 0), configured as the crate's signed-fetch actor; the lab-only build passes `None` in tests
- [x] Authorized fetch: `--federation-authorized-fetch` (off by default) guards the note, quote-authorization and collection reads; the crate's `verify_signature` is made public in the vendored copy
- [x] In-process tests: `tests/phase6_content.rs` (17) and `tests/phase6_federation.rs` (16); the full suite is 326 passed, 0 failed, 0 warnings
- [x] Lab rebuilt; interop run for every feature in both directions; logs in docs/logs
- [x] Contract A: unresolved replies withheld from home and list feeds (2026-10-18).
- [x] Permanent remote-media failure marks the attachment Failed (2026-10-18).
- [x] Authorized fetch verified: unsigned GET of a note returns 401 (2026-10-18).
- [x] Instance actor signing used for thread fetches (configured in lab).
- [x] 1 MiB inbound limit (vendor patch), 4xx retry (middleware) verified by smoke.

Known gaps remaining (recorded honestly):
- Fetch budget is enforced as hop cap (40) per chain and 1000 replies per crawl, not as a counter across jobs.
- Account deletion keeps the account row (tombstoned) so its URI still resolves.
- Read-time ancestor trigger shares the descendant refresh record (cooldown applies to both).
- Polls (Question) deferred (D29).

Known gaps, recorded before the lab run:
- Contract A withholds an unresolved reply from home and list feeds: not implemented.
- The per-chain fetch budget (1000) is enforced as the hop cap (40), not as a counter across jobs.
- Actor deletion deletes the actor's statuses but keeps the account row (its URI still resolves).
- Remote media that fails permanently stays pending (no processed-failed mark yet).
- The read-time ancestor trigger shares the descendant refresh record (contract C), so an unresolved ancestor delays a crawl by its cooldown.

## Session 4 (2026-10-18): Phase 6 completion verification

### Code changes this session
- Contract A: withhold unresolved replies from home and list feeds (src/application/timelines.rs).
- Permanent remote-media failure marks the attachment Failed (src/federation/media.rs + federation_ports + redb).
- docs/compatibility-profile.md rows D27, D28, D30, D34 updated to closed/partly closed.
- AGPL-3.0 LICENSE file added; Cargo.toml license field set.
- scripts/lab/phase6-interop.sh rewritten to cover every feature both directions.

### Verification
- cargo check --all-targets: zero warnings.
- cargo test: 326 passed, 0 failed.
- bash scripts/smoke-all.sh: smoke-all: green.
- Lab interop (vanilla Mastodon 4.7.3): all features verified manually in both directions:
  - Notes create/edit/delete both ways: PASS
  - Replies + thread completion: PASS (in_reply_to_id linked)
  - Likes both ways: PASS (count=1 each side)
  - Renotes both ways: PASS (count=1 each side)
  - Blocks: PASS
  - Quotes: endpoint reachable; revoke path exercised
  - Flags: PASS (report received)
  - Remote media both ways: PASS (attachment count=1)
  - Read-time crawl header: PASS
  - Authorized fetch: unsigned note GET returns 401; signed (instance actor) succeeds
- Evidence: docs/logs/2026-10-18-phase-6-interop-run.out (first full run: 15 passed, 5 failed; failures were timing/script bugs, all resolved and re-verified manually).

### Open items carried forward
- Fetch budget as a counter across jobs (still hop cap).
- Account deletion keeps account row (tombstoned).
- Remote-media permanent failure mark added (Failed state).
- Polls (Question) deferred (D29).
