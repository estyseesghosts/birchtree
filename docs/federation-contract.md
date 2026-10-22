# docs/federation-contract.md

Status: Draft

ActivityPub federation contract (per `birchtree.md` §2.7 and §6). Federation is delegated to the `activitypub-federation-rust` crate; this document describes the boundary between birchtree's application layer and that crate, not a hand-rolled ActivityPub implementation.

Mastodon behavior cited here is verified against the pinned tag: `git -C ~/fedifedifedi/mastodon show v4.7.3:<path>` (the reference checkout's working tree is on `main` and must not be used for these claims).

## Ingress (bounded protocol adapter)

1. Authenticate HTTP signature + digest.
2. Parse and validate untrusted JSON-LD/ActivityStreams (size limits, content validation).
3. Deduplicate activity IDs (canonical activity URI dedup record).
4. Apply exactly one application command.
5. Persist follow-up jobs in the same transaction.
6. Return the appropriate acknowledgement.

Inbound failures never roll back committed local actions.

### Remote custom emoji (federation tail, G-D; D32c)

`Emoji` tags on Notes (`name` `:shortcode:`, `icon` image URL) parse in
`note_input_for` to namespaced refs (`shortcode@domain` via
`remote_emoji_shortcode`, so origins never collide and local rows win).
Rows persist in the `REMOTE_EMOJI` table (schema v20) and stay enabled but
picker-invisible: they never join the public index, and status-payload
`emojis` arrays resolve them per author's domain. Bytes arrive post-commit
through the durable `FetchRemoteEmoji` job (`fetch_remote_emoji_bytes`),
under the same SSRF/size (256 KB)/PNG-GIF-WebP-sniff caps as remote media
(`src/federation/media.rs`); refusals quarantine the job without failing the
status. Evidence: `tests/gaps_federation_tail.rs` (19 tests). Account-payload
emoji remain `[]` (open on both sides).

## Egress (durable per-destination delivery)

- Commit delivery jobs before delivery.
- Sign the correct request (HTTP signature), correct digest.
- Retry per destination with backoff/cooldown; detect permanent failures; terminal quarantine.
- One failing host must not block unrelated delivery.
- Safe HTTP client: DNS/IP SSRF defenses, redirect/time/size limits, content validation.

### Per-destination failure budget and cooldown (`src/federation/delivery.rs`)

Each destination host carries a `DeliveryDestState` (`failures`, `cooldown_until_ms`, table `delivery_dest`): consecutive failed attempts to one host move its cooldown out exponentially (`DEST_COOLDOWN_BASE_MS` 60 s, doubling, capped at `DEST_COOLDOWN_CAP_MS` 1 h). A job aimed at a host inside its cooldown is parked — completed now and re-enqueued as a deferred copy due when the cooldown ends — instead of hitting the down host again. Past `DEST_FAILURE_BUDGET` (10) consecutive failures, or `DEST_MAX_DEFERRALS` (25) parks, the job quarantines. A 2xx resets the host. Only a retryable send failure charges the host's budget: a permanent refusal (refused URL, missing key, unparseable payload) quarantines the job without cooling down its host, so one bad inbox never penalizes its neighbours. Inboxes on one host share the budget and the cooldown (the failure domain is the host, not the path). Evidence: `tests/phase6_gaps_round2_e.rs` (failing host parks with no second hit; unrelated delivery unaffected; success resets; budget and deferral cap quarantine).

### Per-destination FIFO sequence (schema v23, prod-2 item 9)

Concurrent workers must not reorder a `Follow`/`Undo` (or
`Create`/`Delete`) pair to one inbox on the wire. Every `DeliverActivity`
(and `RelayForward`) enqueue mints the destination host's next monotonic
number (`DELIVERY_SEQ` table) into the payload as `seq` — in the same
transaction for in-transaction planning, one call apart otherwise — so plan
order and sequence order are identical and survive restarts. The send path
holds the per-host lock and then parks sequence N+1 (deferred copy, no
retry budget consumed, 5 s park delay) while any earlier sequence for the
same host is still `Pending` or `Leased`. Terminal rows (`Succeeded`,
`Quarantined`) never block: a permanently failed earlier send must not
wedge its host. Different hosts proceed in parallel.

Evidence: `tests/fix_delivery_order.rs` (second send parks while the first
is leased and the pair arrives in order; order survives a mid-queue
restart with the counter continuing; a quarantined earlier sequence does
not wedge its host).

### Delete-beats-Create tombstones (`src/federation/tombstone.rs`, table `remote_tombstone`)

Upstream files a `Tombstone` row when a `Delete` arrives, even for an object never seen, guarded by the same-host check (`delete.rb`: `Tombstone.find_or_create_by`, `non_matching_uri_hosts?`). Birchtree matches it (each store call is one short transaction; a `Create` that wins the race is caught the other way — the `Delete` then lands on the stored row):

- Inbound `Delete` for an object URI with no status row files a tombstone, same-host only (an actor may only tombstone its own host's objects, so a forged `Delete` cannot poison arbitrary URIs). A URI with a row, or with a tombstone already, is a duplicate; another host's URI is ignored (and, per D33, not recorded as seen).
- Every `Create`-shaped ingest path (inbox `Create`, `Update`-as-`Create`, quote instruments, object dereference via `NoteRef::from_json`) checks the tombstone first: a tombstoned URI is dropped as a duplicate (the dereference path reports it deleted, which is terminal for the fetcher) instead of resurrecting it.
- Outbound: a locally deleted status keeps its `STATUS_BY_URI` mapping on the tombstoned row, so a redelivered or relayed `Create` for the same URI is a duplicate and the content is untouched. Deleted local objects are served as `Tombstone` documents (410) on the object route.

Evidence: `tests/phase6_gaps_round2_e.rs` (Delete-before-Create both directions, cross-host poisoning refused, edits of tombstoned URIs ignored).

### Remote document limits (dereference path through the safe client)

Document fetches (actor/object/collection dereference) go through the crate's client built in `federation::net` (SSRF resolver, no reqwest redirects, connect + total timeouts, URL policy) with the `FailClientErrors` rule. The crate enforces the wire limits; birchtree pins and tests them:

- Body cap 200 KB (`MAX_BODY_SIZE` in the vendored shim): an oversize document is rejected (`ResponseBodyLimit`), never buffered.
- Content validation: only ActivityStreams content types are accepted (`FetchInvalidContentType` otherwise); the object's `id` must match the final URL (`FetchWrongId`). (The vendored fetch parses the body before the content-type check, so a non-JSON body fails earlier with `ParseFetchedObject` — terminal too.)
- Statuses pass through on fetches: the 4xx-to-error rule (D28) applies to delivery POSTs only. A 410 maps to `ObjectDeleted` and a 404 to a parse failure — each terminal for that URI (no retry loop) — instead of every 4xx becoming a retried middleware error.

Evidence: `tests/phase6_gaps_round2_e.rs` (oversize/wrong-content rejected, 410 terminal, 404 passes through; wrong-content parent fetch leaves the reply unresolved with no re-enqueue).

### Delete distribution and tombstones, local side (D38, 2026-10-20)

Upstream: `DeleteAccountService` removes the account's statuses in batches (`BatchedRemoveStatusService`, slices of 50) and sends a `Delete` of the actor (`ActivityPub::DeleteActorSerializer`) to follower inboxes (plus relays, which do not exist here); a single status delete sends a `Delete` of the Note. Birchtree matches that shape:

- Local status delete (single or via the deletion worker): one `FederationEvent {event: "delete", status_id}` committed in the same transaction as the tombstone (`StatusStore::delete_status`), fanned out per destination inbox by the event worker as `Delete {object: {id, type: Tombstone, formerType: Note}}` (`src/activitypub/outgoing.rs`, `src/federation/events.rs`).
- Local account deletion (`DELETE /api/v1/admin/accounts/:id`, `reject`): suspend + disable immediately, plus — in the same transaction — the first `AccountDeletionBatch {account_id, after}` job and a `FederationEvent {event: "delete_actor"}`. The batch chain tombstones 50 statuses per job (each with its own `Delete` event) and ends on a short page; the actor event goes to follower inboxes as `Delete {object: {id, type: Tombstone, formerType: Person}}`.
- Tombstone rule (outbound side): rows are tombstoned, never hard-removed — status rows keep `deleted_at_ms`, the account row stays suspended — so deleted URIs keep resolving (as tombstones) instead of dangling, and a retried batch can never resurrect or duplicate a status. Delivery is at-least-once per destination under the job retry budget, then quarantine; duplicates are idempotent downstream (activity-ID dedup).

### Profile update distribution, local side (2026-10-21)

Upstream: any profile write enqueues `UpdateDistributionWorker` (debounced ~5 s), which sends an `Update` of the actor (`ActivityPub::UpdateActorSerializer`, the full actor document) to the account's remote followers (via `AccountReachFinder`: followers plus reporters, recent mentions/follows). Birchtree matches that shape without the debounce:

- `AccountStore::update_account_profile` commits one `FederationEvent {event: "profile_update"}` in the same transaction as the account row (a no-op for remote rows, which never reach this path).
- The event worker rebuilds the actor document through the same `into_json` the served `Person` endpoint uses (so refetch and fan-out can never disagree) and sends `Update {object: <full actor document>}` (`src/activitypub/outgoing.rs::profile_update`, `src/federation/events.rs`) to the account's remote follower inboxes. The served document carries `icon`/`image` only while the account references a staged avatar/header blob.
- Intended divergence: immediate enqueue (no 5 s debounce) and follower-only recipients (no reporters/mentions expansion). Evidence: `tests/avatar_upload.rs` (`multipart_avatar_and_header_upload_round_trip`, `profile_update_event_carries_the_new_actor_document`).

## Initial activity set (Phase 6)

Create/Note, Update, Delete, Follow/Accept/Reject/Undo, Like/Undo, Announce/Undo, Block, quote Request/Accept/Reject/Revoke, and Flag (send and receive). Flag is required by reports (Phase 7.3); local report creation does not depend on it.

## Discovery

WebFinger `acct:` resolution, NodeInfo, actor and key documents, inbox/shared inbox/outbox, object dereferencing, canonical URI mapping.

## Signed fetch and authorized fetch

- **Default signing.** Outbound dereferences are signed with the instance actor's key when no `on_behalf_of` actor is passed. Mastodon's `fetch_resource_without_id_validation` sets `on_behalf_of ||= Account.representative`, so every default fetch in the thread-completion path is signed. Birchtree needs an instance-actor keypair before Phase 6 thread completion ships.
- **Authorized fetch (operator setting).** When enabled (Mastodon: the `AUTHORIZED_FETCH` environment variable or the `authorized_fetch` setting, or limited federation mode), birchtree's replies, context, collection, likes, shares, outbox, quote-authorization, featured-collection and feature-authorization endpoints require a valid HTTP signature. Mastodon enforces this with `require_account_signature!` on those controllers.
- **Consequence.** A remote server in authorized-fetch mode rejects unsigned dereferences. Thread completion must send only signed requests.

## Remote thread completion (Phase 6)

When a cached remote status has an unknown parent, or a replies collection that was never crawled, Mastodon fetches the missing statuses so an older remote conversation becomes viewable. Birchtree must match the observable outcome while keeping every safety rule in `birchtree.md` §4.2.

### Upstream mechanism (Mastodon v4.7.3)

| Trigger | Mechanism | Bounds | Pinned source |
|---|---|---|---|
| Inbound `Create` of a reply whose parent is unknown | `resolve_thread` enqueues `ThreadResolveWorker` (queue `pull`, retry 3). The worker returns if the child already has `in_reply_to_id`; looks the parent up locally; otherwise fetches it with `FetchRemoteStatusService`; sets `thread`; enqueues distribution if the child is inside the realtime window. A nil parent returns silently (no re-enqueue). | One hop per job. `depth` is not passed on this path. Ingest guard: 1000 discoveries per `request_id` (Redis key, 5-minute TTL). | `app/lib/activitypub/activity/create.rb` (`resolve_thread`), `app/workers/thread_resolve_worker.rb`, `app/services/activitypub/fetch_remote_status_service.rb` |
| Inbound `Create` with an embedded `replies` collection | `fetch_replies` calls `FetchRepliesService` with `allow_synchronous_requests: false`; if that yields nothing, it enqueues `ActivityPub::FetchRepliesWorker`. | 5 replies per call (`FetchRepliesService::MAX_REPLIES`). Same-host filter (`filter_replies` → `non_matching_uri_hosts?`). | `create.rb` (`fetch_replies`), `app/services/activitypub/fetch_replies_service.rb` |
| Authenticated `GET /api/v1/statuses/:id/context` on a remote, distributable root | Answers from local state, then sets `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]` and enqueues `ActivityPub::FetchAllRepliesWorker` in a batch. Anonymous viewers never trigger a fetch. | Root must be remote and distributable, created at least 5 minutes ago (`FETCH_REPLIES_INITIAL_WAIT_MINUTES`), and not refreshed in the last 15 minutes (`FETCH_REPLIES_COOLDOWN_MINUTES`). If a refresh is already running for the status, the header carries its id and nothing new is enqueued. | `app/controllers/api/v1/statuses/contexts_controller.rb`, `app/models/concerns/status/fetch_replies_concern.rb`, `app/controllers/concerns/async_refreshes_concern.rb` |
| Crawl | `FetchAllRepliesWorker` fetches the root body and enqueues a `FetchReplyWorker` prefetch for it. It reads the root's `replies` collection through `FetchAllRepliesService`, then does a breadth-first crawl over each fetched reply's own `replies`, deduplicating by URI. Each kept URI becomes a `FetchReplyWorker` job. | 500 items per `FetchAllRepliesService` call. 1000 fetched URIs total (`FetchAllRepliesWorker::MAX_REPLIES`). 500 collection pages total (`MAX_PAGES`). Statuses created within 5 minutes or refreshed within 15 minutes are skipped. | `app/workers/activitypub/fetch_all_replies_worker.rb`, `app/services/activitypub/fetch_all_replies_service.rb` |

Read path, for comparison: `GET /api/v1/statuses/:id/context` returns ancestors and descendants from local state only (a recursive walk over `in_reply_to_id`). Anonymous viewers get at most 40 ancestors, 60 descendants and depth 20. Authenticated viewers get up to 4096 of each.

Every default fetch in this path is signed by the instance actor (`Account.representative`, see "Signed fetch" above). Ingest guards in `FetchRemoteStatusService#call`: a disallowed domain returns nil (`domain_not_allowed?`); the object's host must match its `attributedTo` host (`trustworthy_attribution?`); a suspended actor returns nil; the Redis discovery budget applies.

Read-side withholding: `FeedManager#filter_from_home` (`app/lib/feed_manager.rb`) keeps a reply whose `in_reply_to_id` or `in_reply_to_account_id` is nil out of home and list feed insertion, except the author's own statuses.

### What the pinned source does not do

These differ from the first plan text and are recorded as corrections. Birchtree's plan follows the source where the two disagree, unless a deviation is decided and recorded.

1. **Mastodon does not dereference missing ancestors on read.** Ancestors resolve at ingest, through a chain of separately enqueued `ThreadResolveWorker` jobs, one hop each. The read path (`context`) only completes descendants, and only for authenticated viewers.
2. **Ancestor chains have no depth cap in the ingest path.** `depth` is threaded only through `VerifyQuoteService`. The only bound is the 1000-discovery budget per `request_id`, and each hop re-enters `Create`.
3. **The all-replies crawl skips the same-host filter.** `FetchAllRepliesService` overrides `filter_replies` without calling `super`, so the inline path's same-host rule does not apply to the crawl.
4. **Withholding is limited to feed insertion.** The verified withholding is home and list feed insertion. I did not verify a read-side filter for direct or context reads of unresolved replies.

### Birchtree behavior contract

**A. Ingest-time parent resolution (Mastodon-matching).**

- Trigger: an inbound reply is stored with no resolved parent and a valid parent URI. Enqueue a durable job `ResolveParent { child_status_id, parent_uri, hop, budget_id }` with `hop = 0`.
- Each job: skip if the child is already resolved. Look the parent up locally by URI. If it is absent, fetch it through the safe HTTP client, signed by the instance actor. If the fetched parent is itself an unresolved reply, enqueue the next hop with `hop + 1`.
- The link (`in_reply_to`, thread) commits in one short transaction with any counters it affects. Fetching happens outside that transaction.
- Outcomes: resolved → linked; not found, blocked, unsafe, or budget exhausted → terminal, the reply stays unresolved, no re-enqueue; transient error → bounded retry with backoff, then quarantine.
- Never promote an unresolved reply to root. Never create a synthetic parent.
- The fetched parent must pass the same policy as any remote status: domain blocks, suspended actors, `trustworthy_attribution`.
- Caps are proposals, mirroring upstream constants where they exist: `MAX_PARENT_HOPS = 40` per chain (no upstream hop cap exists), and a fetch budget of 1000 per triggering request (mirroring upstream `DISCOVERIES_PER_REQUEST`).
- Until linking succeeds, withhold the reply from home and list feed insertion (matching upstream `filter_from_home`), and from ordinary reads per `birchtree.md` §4.2, unless the pinned behavior demands a different explicit projection.

**B. Read-time descendant completion (Mastodon-matching).**

- Trigger: an authenticated `GET /api/v1/statuses/:id/context`. Anonymous reads never trigger fetches.
- Eligibility: the status is remote and distributable, was created at least 5 minutes ago, and has not been refreshed in the last 15 minutes.
- If eligible and no refresh is running: respond `200` with the local context, set `Mastodon-Async-Refresh: id="<refresh-id>", retry=3[, result_count=<n>]`, and enqueue a durable crawl job. If a refresh is already running for the status: set the header with that refresh's id and enqueue nothing.
- The crawl job reads the root, then the root's `replies` pages (at most 500 items per collection and 500 collection pages), then breadth-first over each fetched reply's `replies`. It stops at 1000 fetched replies in total, deduplicates by URI, and skips statuses inside the refresh cooldown.
- Crawl fetches are signed and use the same SSRF, size, time and redirect rules as every other remote fetch.
- Same-host rule: none, matching upstream. The crawl follows replies on any host, as upstream's all-replies crawl does (see "What the pinned source does not do", item 3). The crawl's cost is bounded by the caps in D, and every fetch still passes the SSRF, size, time and redirect rules.
- Completion: the refresh record is marked done. The next context read returns the newly linked replies. The header's `retry=3` tells clients when to poll.

**C. Read-time ancestor completion (decided 2026-10-17; deviation D5).**

- Trigger: the same authenticated `GET /api/v1/statuses/:id/context` as B. Anonymous reads never trigger it.
- Mechanism: walk up from the status. The first ancestor whose parent is known by URI but not stored locally gets a `ResolveParent` job (A), with `hop = 0`. Each resolved hop enqueues the next unresolved link, so the chain is completed one hop per job, under the caps in D.
- No initial wait: unlike replies (5 minutes after creation), an unresolved parent is resolved as soon as a logged-in viewer asks. Each status's refresh is still subject to the 15-minute cooldown, so repeated reads do not re-enqueue.
- Shared refresh: ancestor and descendant completion share one refresh per status. The `Mastodon-Async-Refresh` header is set when either kind of job is enqueued, or when a refresh is already running.
- Response: the context is answered from local state at request time, as upstream does. Completed ancestors appear on the next read (`retry=3`).
- Deviation: upstream never fetches missing ancestors on read (see "What the pinned source does not do", item 1). birchtree does, for logged-in viewers only. The register in `docs/compatibility-profile.md` records it as D5 and the operator's acceptance.

**D. Limits (summary).**

| Limit | Value | Source |
|---|---|---|
| Parent hops per chain | 40 (proposed) | birchtree proposal; upstream has none |
| Fetch budget per triggering request | 1000 | upstream `DISCOVERIES_PER_REQUEST` |
| Replies per collection call | 500 | upstream `FetchAllRepliesService::MAX_REPLIES` |
| Replies fetched in total per crawl | 1000 | upstream `FetchAllRepliesWorker::MAX_REPLIES` |
| Collection pages per crawl | 500 | upstream `FetchAllRepliesWorker::MAX_PAGES` |
| Inline replies from an embedded collection | 5 | upstream `FetchRepliesService::MAX_REPLIES` |
| Initial wait before a status is crawled | 5 minutes | upstream `FETCH_REPLIES_INITIAL_WAIT_MINUTES` |
| Cooldown between crawls of one status | 15 minutes | upstream `FETCH_REPLIES_COOLDOWN_MINUTES` |
| Refresh poll hint | `retry=3` | upstream `add_async_refresh_header` default |

**E. Security and failure.**

- Every thread fetch (parent, reply, root, collection page) goes through the safe HTTP client: DNS/IP SSRF checks, redirect re-validation, size and time limits, content validation.
- The crawl is the widest-reach path in this contract: a remote server can name up to 1000 reply URIs, on any host. The caps in D, per-URI dedupe, the cooldown and the SSRF rules bound it. The cross-host reach matches upstream by decision (2026-10-17).
- No outbound HTTP inside a local status or follow transaction.
- Jobs are idempotent and deduplicated by object URI; a retried job cannot duplicate a link or a status.
- An unauthorized or refused fetch (401, 403, 404, 410) is terminal for that URI. It is not retried in a loop.

**F. Interop tests required before Phase 6 exit** (suites in `birchtree.md` §8, item 7).

- Unknown parent resolved to completion across two servers; chain longer than the hop cap stops with the reply unresolved.
- Parent 404 or blocked: terminal, reply unresolved, not promoted.
- Authorized-fetch remote: signed fetch succeeds; unsigned fetch is never sent.
- Cross-host reply URI in a replies collection: fetched under the caps and the SSRF rules, as upstream's crawl does. The inline path still rejects it (same-host filter).
- Anonymous context read: no job enqueued, for ancestors or descendants.
- Logged-in context read with an unresolved ancestor chain: one `ResolveParent` job per hop, chained only after the previous hop resolves; a chain longer than the hop cap stops with the nearest unresolved status left unresolved.
- Ancestor and descendant completion for the same status share one refresh: one `Mastodon-Async-Refresh` id, and no second refresh while one is running.
- Authenticated context read on a fresh remote root: header present, one job; repeat reads within cooldown: header only, no new job.

## Non-goals

No outbound HTTP inside a local status/follow transaction. No multi-node mode until storage adapter + media arrangement + routing + shared conformance suite are implemented and tested. Read-time ancestor completion is in profile for logged-in viewers only (see C, deviation D5).

## Media federation interop notes (bench run5, 2026-10-19/20)

No product change. All run5 media FAILs involving the lab Mastodon node
were investigated with wire + code evidence and found not ours:

- Outbound (`*/bt -> mastodon mismatch:404`): our `Document` emit
  (`src/activitypub/outgoing.rs:130-158`, `type/mediaType/url/name` +
  conditional `blurhash/width/height`, string-form `url`) matches what
  Mastodon 4.7.3 parses (`media_attachment_parser.rb`, `json_ld_helper.rb`
  `url_to_href`); media URLs are unauthenticated
  (`src/api/mastodon/endpoints/media.rs:133-155`,
  `src/api/mastodon/router.rs:270`) over plain `http://` in bench, so no
  TLS/lab-CA involvement. The failing URL is Mastodon's own
  `system/cache/...` path, i.e. Mastodon created the attachment row and its
  fetch/store (or Sidekiq redownload lag) never made bytes servable.
  Independent pinetree sender fails identically. Verdict: MASTODON-SIDE
  (+ possible BENCH-ARTIFACT transient).
- Inbound (`mastodon -> * mismatch:200`): our remote fetch stores original
  bytes verbatim with sniffed MIME (`src/federation/media.rs:65-91`) and
  re-serves them unmodified with re-sniffed content-type
  (`src/api/mastodon/endpoints/media.rs:133-151`); control pairs
  (`pt/bt -> bt`) are byte-exact PASS. Mastodon re-encodes every locally
  uploaded image (`lazy_thumbnail.rb needs_convert?` /
  `media_attachment.rb GLOBAL_CONVERT_OPTIONS`), so a byte-hash oracle
  (`sha(uploaded) == sha(served)`) can never pass for mastodon-src pairs.
  Verdict: MASTODON-SIDE + HARNESS-TOO-STRICT (needs perceptual or
  served-snapshot comparison).
- `no-attachment` (dst=mastodon): 0/34 media deliveries into lab-mastodon
  across five independent senders (bt, pt, misskey, catodon, akkoma) with
  zero non-mastodon failures; interop vs real Mastodon 4.7.3 passes
  (`interop.out` "mastodon received media from bt-a PASS"). Fetch failures
  on Mastodon keep the attachment row (→ 404 shape), so zero-record rows
  are a mastodon-ingest-side drop, mechanism unproven (lab Sidekiq/rails
  logs not retained). Verdict: MASTODON-SIDE / BENCH-ARTIFACT.
- Bench media payloads are PNG-only, so video/GIF `Document` emit (same
  code shape, only `mediaType`/count differ) is unexercised over
  federation. Cosmetic only: we emit `"name": ""` where Mastodon omits
  null `name` (harmless: `.presence` → nil).
- Phase 7.1 transcodes local uploads (GIF → WebM/VP9 typed `video`, video → WebM/VP9 + Opus, stills → WebP; `gifv` never produced). Remote peers therefore receive attachment bytes, `url`s and `meta` that differ from what the uploading client sent, and remote caches keyed on the original bytes will miss. This is a deliberate, registered deviation (D1–D4): bytes legitimately change. Transcoding never alters visibility, ownership, or attachment ordering — only the stored bytes, wire `type` (GIF `image` → `video`), and derived `meta` — so quote authorization, fetchability, and thread linkage are unaffected.
