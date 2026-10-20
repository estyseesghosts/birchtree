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

## Egress (durable per-destination delivery)

- Commit delivery jobs before delivery.
- Sign the correct request (HTTP signature), correct digest.
- Retry per destination with backoff/cooldown; detect permanent failures; terminal quarantine.
- One failing host must not block unrelated delivery.
- Safe HTTP client: DNS/IP SSRF defenses, redirect/time/size limits, content validation.

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
