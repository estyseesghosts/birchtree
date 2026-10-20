# Rust Mastodon-Compatible Server — Revised Implementation Plan

**Revised:** 2026-10-16  
**Reference release:** Mastodon **4.7.3** (released 2026-10-01)  
**Compatibility bar:** an unmodified Mastodon client connects to a birchtree instance and has an identical experience to connecting to Mastodon v4.7.3 (API v7 contract). Parity is built up phase by phase, with deliberate differences listed in the deviation register (`docs/compatibility-profile.md`); it is not claimed on day one  
**Initial deployment:** one Rust process, `redb`, local media storage, in-process durable-job workers  
**Frontend:** birchtree has no frontend source code in this repository. It requires a minimal server-rendered login/consent flow for Mastodon-compatible OAuth authorization, and it serves **keystodon**, the frontend developed in parallel in a separate project. keystodon runs in two modes. As the **first-party client**, birchtree serves its built bundle and keystodon authenticates with birchtree's browser session plus CSRF on `/api/web/*` (§7 Phase 7). As a **third-party client**, keystodon authenticates against any host, including birchtree, through OAuth (§6.3). keystodon is not on the development machine yet. It is the planned interoperability target for both modes (§8).

This plan is based on Pinetree’s latest repository state and a source-level review of Mastodon v4.7.3. The pinned Mastodon tag is the compatibility authority; `main` and unversioned documentation are useful for detecting later changes, not for silently changing this target. [v4.7.3 changelog](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md) · [release list](https://github.com/mastodon/mastodon/releases)

## 1. Decisions and compatibility definition

Build a **separate Rust project**. Carry over Pinetree’s architectural rules and verification discipline, not its Misskey domain or API shapes.

- **One modular monolith.** Begin with one Cargo package. Split into workspace crates only when there is a measurable reason.
- **Domain → application → adapters.** Domain code knows neither HTTP DTOs nor ActivityPub JSON nor `redb`. REST and ActivityPub must call the same application use cases.
- **`redb` for a single-node server.** No SQL, Redis, or external broker requirement. FoundationDB/cluster mode is not part of the initial implementation.
- **Atomic state + durable follow-up work.** In one `redb` transaction, commit each state mutation, affected indexes/counters, idempotency/dedup records, and the bounded durable jobs that must follow it. Do network I/O, media processing, notifications fan-out, and federation delivery after commit.
- **Mastodon-compatible decimal IDs.** Implement a 64-bit timestamp-oriented ID generator and serialize IDs as decimal strings, matching the numeric ID shape and ordering assumptions in Mastodon. Do not expose UUIDs or Pinetree’s AIDX format. Keep comparisons numeric internally; never order decimal IDs lexicographically.
- **Contract first.** Pin endpoints, methods, request encodings, scopes, response fields/nullability, status codes, headers, cursor semantics, streaming events, and unsupported behavior in a manifest backed by tests.
- **No “success-shaped stubs.”** An unimplemented endpoint must be explicitly out of profile and return an appropriate error, not fabricated data.
- **Declared profile scope.** The declared profile includes the admin/moderation API, reports, Web Push subscriptions, conversations, suggestions, announcements, trends, annual reports, donation campaigns, and the `/api/web/*` routes. Each has a planned phase in §7 (Phases 7–7.3). Out of profile: the server-rendered admin dashboard, the Mastodon web app (`/web/*`), and `GET /api/v1/search`, which does not exist at v4.7.3.

### What “drop-in” means here

### The compatibility bar

**An unmodified Mastodon client connects to a birchtree instance and has an identical experience to connecting to Mastodon v4.7.3, measured at 98% or better.** This is the bar for every phase. The aim is identical behaviour: the same requests are accepted; the same response bodies, status codes, headers and pagination come back; the same streaming events arrive; and the same errors are shown. The bar is a measured target, not a claim of 100%. birchtree must match at least 98% of the observations in the parity suite (§8 item 11), and the core client flows (sign-in, compose, timelines, notifications, media) must match in full (proposed). Each place where birchtree deliberately differs must appear in the deviation register in `docs/compatibility-profile.md`, with its reason and whether the operator accepted it. A difference that is not in the register is a bug.

“Drop-in” is an end state, not the first milestone. Test the bar against real unmodified clients, chosen in Phase 0. keystodon is one of them, in both its first-party and third-party modes, once it is available on the development machine. A server that implements posting and timelines but lacks scope semantics, media processing, account registration, streaming, or federation is **not** a drop-in replacement.

Track three separately reported outcomes:

1. **Local usable:** clients can register/authenticate, post/read/edit/delete, follow, interact, upload media, and receive correct timelines/notifications.
2. **Federating:** two-server tests pass for supported ActivityPub activities, visibility, delivery retries, remote media, and quote authorization.
3. **Declared API compatibility:** every endpoint in the chosen public Mastodon 4.7.3 profile has a passing contract or a documented intentional exclusion. Do not advertise full compatibility before this gate passes.

## 2. Mastodon source audit: behavior to reproduce

This is not just an endpoint-documentation exercise. The v4.7.3 code has important behavior in services, policies, serializers, models, background workers, and routing.

### 2.1 Request handling and write paths

- [`config/routes/api.rb`](https://github.com/mastodon/mastodon/blob/v4.7.3/config/routes/api.rb) declares a broad surface: accounts and account relationships, statuses and nested interactions, home/list/public timelines, notifications, media, polls, lists, filters, preferences, scheduled statuses, search, instance discovery, streaming redirects, trends, and other APIs. Build the endpoint manifest from this pinned route set plus the official method docs; do not assume a short hand-written route list represents Mastodon.
- [`Api::V1::StatusesController`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/statuses_controller.rb) is a relatively thin adapter: it checks read/write scopes, resolves the reply and quote targets, authorizes them, invokes services, and serializes the result. Match that responsibility split.
- [`PostStatusService`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/post_status_service.rb) does much more than insert a row. It applies defaults and visibility rules, checks idempotency, validates/attaches media, handles quote restrictions, processes mentions/hashtags/links, updates related records, and schedules distribution/notification/federation work. [`UpdateStatusService`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/update_status_service.rb) records edit history and updates attachments/polls/content with follow-up work.
- Reimplement those *behaviors*, not those Ruby classes. A Rust HTTP handler should parse and authorize input, call one application command/query, and map the result to a wire DTO.

### 2.2 Models, policies, and serializers

- [`Status`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/status.rb) mixes separate concerns for visibility, threading, interaction policy, reply fetching, search, snapshots, and safe reblog insertion. Keep these separate in Rust rather than creating one giant `StatusService`.
- [`StatusPolicy`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/policies/status_policy.rb) makes **operation-specific** decisions: showing, quoting, reblogging, favouriting, editing, and deleting are not interchangeable permissions. Blocks, private visibility, mentions, and quote policy affect different actions. Model these as named policy functions and test each one.
- [`REST::StatusSerializer`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/serializers/rest/status_serializer.rb) is viewer-aware. Fields such as `favourited`, `reblogged`, `muted`, `bookmarked`, `pinned`, quote data, and some filtering state depend on the requesting account. It also has a source/raw-text mode. A global cached JSON blob is not sufficient.
- [`REST::AccountSerializer`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/serializers/rest/account_serializer.rb) packs `acct`, identity/profile fields, media URLs, counts, moved accounts, custom emoji, profile fields, and conditional unavailable/moderation state. Exact field presence and `null` versus empty values are part of compatibility.
- Serializers preload associated records and viewer relationship state. Add a page-scoped `StatusViewContext` / `AccountViewContext` that batch-loads authors, media, polls, quotes, relationship flags, and counters. Avoid N+1 reads while preserving per-viewer output.

### 2.3 Visibility is more nuanced than four enum values

[`Status::Visibility`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/concerns/status/visibility.rb) stores `public`, `unlisted`, `private`, `direct`, and an internal `limited` state. The public API accepts/returns the familiar four privacy levels; `limited` is an internal moderation/restricted state and is presented as `private` in the REST serializer. Public and unlisted statuses are distributable; private and direct statuses have different recipient rules. Reblogs cannot use direct/limited visibility.

Do not copy Pinetree’s `public/home/followers/specified` model. Keep separate predicates for:

1. **Can this viewer fetch this status?**
2. **Can it appear in public, local, home, list, hashtag, or search results?**
3. **Can it be delivered to a particular ActivityPub inbox?**
4. **Can this viewer quote, reblog, favourite, reply to, bookmark, or report it?**
5. **Should it be withheld because of blocks, domain blocks, mutes, account unavailability, or filters?**

One domain policy should feed REST reads, timelines, search, notifications, streaming, quote previews, and outgoing federation. Separate named predicates can share lower-level rules; do not force materially different decisions into one boolean.

### 2.4 IDs and persisted relationships

[`db/schema.rb`](https://github.com/mastodon/mastodon/blob/v4.7.3/db/schema.rb) and [`Mastodon::Snowflake`](https://github.com/mastodon/mastodon/blob/v4.7.3/lib/mastodon/snowflake.rb) show numeric `bigint` IDs for core entities. The ID carries a millisecond timestamp component plus sequence data; REST responses serialize it as a string. Preserve numeric ordering and decimal-string output, including pagination behavior.

The source models also show that the social graph is not a few counters on `Account`:

- `Follow` stores `show_reblogs`, notification preference, languages, and URI; locked accounts need pending follow requests and Accept/Reject behavior.
- Favourites, bookmarks, mutes, blocks, domain blocks, lists/list membership, pins/endorsements, notifications, filters, polls/votes, media, tags, mentions, and status/account statistics are separate relationships or records.
- Status and account counts are often denormalized and read frequently. Keep them transactionally consistent for local actions. Treat remote-provided counters as untrusted hints, not authoritative local state.
- Separate the **ActivityPub account/actor** from local login credentials/preferences (Mastodon’s `Account` and `User` are distinct concepts).

### 2.5 Quotes are a first-class state machine

Mastodon 4.7.3’s [`Quote`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/quote.rb), [`PostStatusService`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/post_status_service.rb), [`StatusPolicy`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/policies/status_policy.rb), and [`REST::StatusSerializer`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/serializers/rest/status_serializer.rb) establish that a quote is not merely a `reblog_of_id`:

- Quote state includes `pending`, `accepted`, `rejected`, `revoked`, and `deleted`, plus the quote activity URI, approval URI, local/remote quoted account/status, and legacy status.
- Accepted quote counts are maintained separately. The quoted status is not always serialized: accepted/visible/policy-authorized state matters. A response must not leak a protected target through `quote`, preview content, or URI.
- The REST API exposes `quote`, `quote_approval`, `quotes_count`, a default quote policy, quote listing/revocation, and interaction-policy updates. Status creation supports `quoted_status_id` and `quote_approval_policy`.
- Policies are `public`, `followers`, and `nobody`; private/direct statuses force `nobody`. Private quoted content restricts quote visibility. A direct/private mention quote has additional mention authorization rules.
- Remote quote requests may require an asynchronous `QuoteRequest` → `Accept`/`Reject` exchange; approval is not equivalent to the target being visible to every viewer.
- Mastodon prepends a textual `RE:` link when needed for compatibility. Preserve that fallback without treating it as proof that structured quote authorization succeeded.

Use one internal normalized quote representation for FEP-044f, Mastodon’s quote activities/fields, and legacy inbound representations. Keep the external Mastodon `quote` / `quote_approval` projection distinct from internal protocol provenance. Required API behavior comes from the pinned Mastodon contract, not a Pinetree-only extension.

### 2.6 Timelines and streaming architecture

Mastodon’s [`FeedManager`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/feed_manager.rb) uses Redis sorted sets for bounded, materialized home/list feeds, reblog aggregation/deduplication, filtering, and stream updates. Timeline workers push or remove status IDs; the home timeline can report HTTP `206` with asynchronous-refresh signaling while a feed regenerates. The [`streaming controller`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/streaming_controller.rb) may redirect clients to a separately configured streaming host.

This project deliberately removes Redis and the separate Node streaming service. That is a **different implementation**, so test its behavior and limits rather than pretending the internals are equivalent:

- Start with bounded, indexed fan-out-on-read for home/list timelines; merge ordered per-author ranges without scanning every status and batch-load response dependencies.
- Keep the API/streaming contract independent of feed strategy, allowing a later `redb`-backed materialized-feed index if real benchmarks justify it. Such a cache is rebuildable derived state; durable status changes and required jobs remain authoritative.
- Define explicit behavior for feed refresh, filtering, follow-language preferences, hidden/reblogged statuses, stale IDs, and status deletion. If implementing Mastodon’s `206`/async-refresh behavior, reproduce its headers and retry semantics; otherwise record the observed compatibility difference.
- Implement the documented event envelopes and stream-name parameters, auth/scope rules, reconnect behavior, heartbeats, cleanup, and visibility gating. Mastodon supports SSE and WebSocket, and stream hostname discovery is observable behavior. Rust can host both transports in-process.

### 2.7 Federation and durable work

The v4.7.3 [`ActivityPub::Activity`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/activity.rb) dispatches incoming activity types, while [`StatusParser`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/parser/status_parser.rb) normalizes remote objects and [`DeliveryWorker`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/workers/activitypub/delivery_worker.rb) signs and sends asynchronous inbox deliveries with retry and destination failure tracking. The documented protocol includes Create/Update/Delete, Follow, Like, Announce, Undo, Block, Flag, and quote-related activities.

Implement ingress as a bounded protocol adapter: authenticate request/signature/digest, parse and validate untrusted JSON-LD/ActivityStreams data, deduplicate activity IDs, apply one application command, persist follow-up jobs, and return the appropriate acknowledgement. Egress must commit jobs before delivery, sign the correct request, retry per destination, detect permanent failures, and prevent one failing host from blocking unrelated delivery. Never perform outbound HTTP inside a local status/follow transaction.

## 3. Architecture and repository layout

Start in one Cargo package; these are responsibility boundaries, not a demand for a file per trivial function. Router assembly should be declarative and endpoint files should stay small.

```text
mastodon-rs/
  Cargo.toml
  README.md
  AGENTS.md
  docs/
    architecture.md
    source-audit-v4.7.3.md
    compatibility-profile.md
    endpoint-manifest.md
    domain-contracts.md
    visibility-and-authorization.md
    application-ports.md
    persistence-conformance.md
    federation-contract.md
    streaming-contract.md
    testing.md
    logs/                         # append-only verification evidence
  src/
    main.rs
    lib.rs
    config.rs
    observability.rs
    domain/
      account.rs
      user.rs                    # local authentication/preferences; separate from actor
      status.rs
      visibility.rs
      interaction_policy.rs
      quote.rs
      relationship.rs             # small shared types only; split by behavior as needed
      follow.rs
      block.rs
      mute.rs
      favourite.rs
      bookmark.rs
      list.rs
      notification.rs
      poll.rs
      media.rs
      filter.rs
      id.rs
      error.rs
    application/
      ports.rs
      context.rs                  # dependency assembly only
      accounts/
      auth/
      statuses/
      quotes/
      relationships/
      timelines/
      notifications/
      polls/
      media/
      search/
      lists/
      filters/
      instance/
      scheduled_statuses/
      admin/                      # Phase 7.3: roles, moderation, trends approval, metrics
    api/
      mastodon/
        router.rs
        auth.rs
        errors.rs
        request.rs
        pagination.rs
        rate_limits.rs
        dto/
        endpoints/
          instance.rs
          apps.rs
          oauth.rs
          accounts/
          statuses/
          timelines/
          relationships/
          notifications.rs
          media.rs
          polls.rs
          lists/
          filters/
          search.rs
          preferences.rs
          scheduled_statuses.rs
          streaming_discovery.rs
          conversations.rs
          suggestions.rs
          announcements.rs
          donation_campaigns.rs
          push.rs
          web.rs                    # /api/web/* (session + CSRF)
          web_client.rs             # Phase 7: serves keystodon's built first-party bundle
          trends.rs
          reports.rs
          annual_reports.rs
          admin/
      oauth/
        authorization.rs
        token.rs
        scopes.rs
        pkce.rs
        consent_page.rs           # minimal HTML only; no app frontend
    activitypub/
      actor.rs
      objects.rs
      parser.rs
      serializer.rs
      webfinger.rs
      signatures.rs
      inbox.rs
      outbox.rs
    federation/
      resolver.rs
      delivery.rs
      retry.rs
      dedup.rs
      remote_media.rs
    search/                       # Phase 7.2: embedded search backend behind a port (derived, rebuildable index)
    streaming/
      hub.rs
      auth.rs
      channels.rs
      sse.rs
      websocket.rs
    jobs/
      types.rs
      queue.rs
      worker.rs
      handlers/
    media/
      ingest.rs
      blob_store.rs
      processing.rs
      remote_cache.rs
      transcode.rs                # Phase 7.1: post-commit ffmpeg job (GIF and video → WebM/VP9; images → WebP)
    persistence/
      redb/
        mod.rs
        schema.rs
        accounts.rs
        statuses.rs
        relationships.rs
        quotes.rs
        polls.rs
        media.rs
        notifications.rs
        oauth.rs
        jobs.rs
        migrations.rs
  tests/
    api_contract/
    oauth/
    persistence_conformance.rs
    crash_recovery.rs
    federation_interop/
    streaming/
    client_smoke/
  scripts/
    smoke-all.sh
    bench-local.py
```

**Dependency direction:** domain ← application ← adapters. Keep Mastodon-specific DTOs under `api/mastodon/dto`; keep ActivityPub parser/serializer shapes under `activitypub`. No API handler should directly open a `redb` table.

Pinetree references: [architecture proposal](https://github.com/estyseesghosts/pinetree/blob/master/sns-server-proposal.md), [agent rules](https://github.com/estyseesghosts/pinetree/blob/master/AGENTS.md), [application ports](https://github.com/estyseesghosts/pinetree/blob/master/docs/application-ports.md), [persistence conformance](https://github.com/estyseesghosts/pinetree/blob/master/docs/persistence-conformance.md). Pinetree’s current `src/api/misskey/endpoints/mod.rs` and `src/persistence/redb/mod.rs` are oversized; the new project should avoid repeating that growth pattern. Its FoundationDB adapter is still a stub, so reuse the conformance design rather than assuming clustering already works.

## 4. Domain contracts and invariants

### 4.1 Account and authentication

- `Account` represents the ActivityPub identity and client-visible profile. Store immutable internal identity, case-preserving and normalized username, domain/local status, actor URI, inbox/shared inbox, profile source, fields, avatar/header, flags, move target, and local/remote state.
- `LocalUser`/credential record stores password hash, email/verification state when implemented, login/session/MFA state if supported, and user preferences. Do not conflate an account with its credential or OAuth client.
- Account output requires local `acct` versus remote `username@domain`, profile field formatting/verification, custom emoji, media URLs, unavailable/suspended state, and visibility of follower/following collections.
- Bootstrap the first administrator/account by CLI so server development is not blocked by signup UI. Implement Mastodon-compatible account creation, email confirmation, and signup policy separately before claiming registration compatibility.

### 4.2 Statuses and replies

Status must carry author, source text, sanitized/rendered HTML, created/edited/deleted times, language, visibility, sensitive/CW, reply parent/account, reblog target, quote relationship, media order, poll, mentions, hashtags, application/client metadata, and moderation/search flags needed by the selected profile.

- Preserve original source separately from rendered `content`; expose source endpoints/fields where the Mastodon API requires them.
- Replies form parent-child edges. Implement a direct-child index, cursor-bounded children reads, and exact reply counters. Mastodon’s context endpoint must return the expected ancestors/descendants representation; do not substitute Pinetree’s API shape.
- A remote reply with an unknown parent stays unresolved; never silently promote it to a root. The unresolved state is **temporary**: persisted, bounded parent-resolution jobs actively fetch the missing parent (Phase 6, “Remote thread completion”, `docs/federation-contract.md`), one hop per job. The existing safety rules still apply: never promote to root; bounded work per chain (a hop cap and a per-trigger fetch budget); no unbounded recursion; respect blocks, domain blocks, suspended actors and visibility when fetching and when linking; every hop goes through the safe fetch client with its size, time and redirect limits. A reply whose parent cannot be fetched (not found, blocked, unsafe, budget exhausted) stays unresolved and its job terminates. Until linking succeeds, withhold it from ordinary reads, as Mastodon does for home and list feed insertion (`FeedManager#filter_from_home`), unless the pinned Mastodon behavior demands a different explicit projection.
- Reblogs are separate status records referencing the original status, with author and visibility constraints. Do not collapse reblogs and quote statuses into the same operation.
- Editing stores edit history and updates media/poll/content as an atomic use case plus durable notifications/distribution jobs.
- Status creation needs per-account `Idempotency-Key` support for the documented one-hour retention, including key lookup, result replay, and prevention of duplicate status/outbox work.

### 4.3 Visibility and authorization

Implement policy functions with named inputs: viewer/account state, follow state, mention state, block/domain-block state, status privacy, quote policy/state, and requested operation. Cover at minimum:

- `public`: publicly fetchable and distributable.
- `unlisted`: publicly fetchable but excluded from public timeline distribution.
- `private`: follower-targeted; visibility depends on the viewer’s relationship and mention rules.
- `direct`: mention-targeted; must not leak through public/list/search/stream routes.
- `limited`: internal restricted/moderation state; do not allow user creation of it or expose it as a fifth public API visibility value. Match Mastodon’s `private` projection where required.

Centralize filtering but keep distinct policies for show, reply, reblog, favourite, quote, pin, list inclusion, and delivery. Ensure blocks and domain blocks affect read/interaction/distribution consistently. Mutes and filters may suppress feed/notification display without being equivalent to a privacy boundary. Never use “hidden from this timeline” as the only authorization check.

### 4.4 Relationships, counters, polls, notifications

- Follow rows preserve `show_reblogs`, `notify`, language preferences and remote URI; locked-account follow requests support pending/accept/reject and federation activities.
- Store follows, blocks, mutes (including expiry/notification behavior), favourites, bookmarks, pins/endorsements, account notes, list membership and filter records independently with unique keys/indexes.
- Polls hold options, multiple-choice/hide-totals rules, expiry, votes, and aggregate counts; test vote changes, repeat votes, closed polls, hidden totals and federation updates.
- Notifications are durable recipient/activity relationships with types and actor/status context, read/clear/dismiss state, and whatever grouping behavior the 4.7.3 API exposes. Deduplicate repeated incoming activity and avoid duplicate notifications when jobs retry.
- Local counters and corresponding relationship changes commit atomically. Use separate counter records/indexes if it avoids rewriting large primary records, but do not create eventually incorrect local counts.

## 5. `redb` storage and performance design

Mastodon’s `db/schema.rb` shows the effective access pattern despite PostgreSQL specifics: snowflake IDs, separate `status_stats`/`account_stats`, relationships, quotes, polls, media, notifications, tokens, lists, tags, and many indexes. Translate those access patterns; do **not** port its Rails schema or its SQL/Redis assumptions literally.

### Required record tables

Accounts, local users/credentials, statuses, follows, follow requests, blocks, domain blocks, mutes, favourites, bookmarks, pins/endorsements, account notes, lists/membership, media metadata, polls/options/votes, quote records, mentions, tags, custom emoji, notifications, OAuth applications, authorization codes, access tokens, activity/actor/key cache, inbound activity dedup, outbound deliveries, durable jobs, and schema/migration metadata.

### Required indexes for first use cases

- unique normalized local handle; handle + domain lookup; actor URI lookup;
- status ID and canonical URI; statuses by author and descending ID; reply by parent and child ID; reblog target and quoting status ID;
- public/distributable timeline range; local-status range; hashtag-to-status range;
- follows both directions; follow requests both directions; blocks/mutes both directions; favourites/bookmarks by account and status; lists and membership both directions;
- notifications by recipient + descending ID and activity identity;
- media by owner and processing state; poll by status and votes by poll/account;
- quotes by quoted status/state and by quoting status; OAuth app/token/code lookup;
- jobs by due time + ID, lease and terminal/quarantine state; delivery jobs by destination/due time; dedup by canonical activity URI.

Use composite, ordered keys where they support bounded prefix/range reads. Avoid global scans, unbounded page filtering, and N+1 packer fetches. Store serialized domain records as values where practical, but do not expose a generic arbitrary-query port or `redb` table definitions to application code. Each migration/index backfill must be versioned, bounded, restartable, and idempotent.

### Mutation protocol

For status create/update/delete, follow/unfollow, favourite/bookmark, poll vote, quote decision/revoke, and inbound ActivityPub mutation:

1. Parse input and do slow/remote lookups before the write transaction when safe.
2. In one short transaction, re-check mutable invariants and commit primary records, indexes, local counters, dedup/idempotency records, and bounded durable jobs together.
3. Process media conversions, notifications, streaming publication, and outbound federation after commit. Every worker is idempotent and settles its lease by success, retry, defer, or terminal quarantine.
4. Crash tests must prove no committed mutation can lose its required job; a failure before commit must leave neither the mutation nor its job.

Do not put media bytes, HTTP calls, follower lists, or unbounded fan-out into a transaction. File/object storage cannot participate in a `redb` transaction: stage incoming bytes, validate/process them, commit metadata last, and run bounded orphan cleanup after crash. Keep the authoritative media state explicit (`pending`, `processing`, `ready`, `failed`) so the API can return Mastodon-compatible processing status.

### Media transcoding (Phase 7.1)

Transcoding is post-processing, not part of the upload request. An upload commits its metadata in the existing `pending`/`processing` lifecycle. A durable transcode job then runs ffmpeg through a configured binary path, outside any transaction, writes derived blobs to staging, and commits the derived-blob references and the resulting state last. A crash leaves at most orphaned staged blobs, which bounded cleanup removes.

Transcoding covers three inputs. GIFs convert to **WebM with VP9** and are typed `video`. Video converts to WebM with VP9 and Opus audio, except that VP9 video with Opus audio or no audio within the upload size limit keeps its codecs (byte-identical when already WebM, stream-copied into WebM otherwise). Still images convert to **WebP**, except an image already in WebP, which passes through unchanged. The server never strips or rewrites EXIF or other metadata (§7 Phase 7.1). Upstream converts animated GIF to MP4 and reports `type: gifv` (`lib/paperclip/gif_transcoder.rb` at v4.7.3). birchtree never produces `gifv`. That is a deliberate deviation, recorded in `docs/endpoint-manifest.md`. ffmpeg is an external runtime dependency: when transcoding is enabled, a missing or non-working ffmpeg is a startup configuration error, never a silent pass-through. Each transcode job runs under wall-time, output-size and memory limits.

### Timeline strategy: explicit trade-off

Mastodon relies on Redis-backed feed materialization and background distribution. The first Rust implementation should **not** rebuild that architecture by stealth or make an unbounded fan-out-on-read query. Start with bounded indexed reads and a k-way merge over relevant followed-account ranges, with explicit scan budgets and page-level preload. Benchmark with high-follow-count accounts and sparse/filtered posts. If it fails the latency target, add a durable, rebuildable `redb` home/list feed index populated by outbox jobs; keep its semantics and recovery contract identical to fan-out-on-read. This is a performance milestone, not permission to weaken compatibility.

## 6. HTTP, OAuth, API and wire compatibility

### 6.1 Generate the contract from v4.7.3

Before implementing handlers, inventory all user-facing routes and methods from [`config/routes/api.rb`](https://github.com/mastodon/mastodon/blob/v4.7.3/config/routes/api.rb), cross-check each against the official [Mastodon API documentation](https://docs.joinmastodon.org/), and create one manifest row per method/path. Track:

- auth mode and minimum scope;
- query/form/JSON/multipart fields, defaults, bounds and nested-array conventions;
- method and response status (including `202`, `206`, `204`, `401`, `403`, `404`, `422` where applicable);
- JSON fields, type, nullable/conditional/omitted semantics and identifier format;
- `Link`, `Retry-After`, rate-limit, async-refresh and streaming-discovery headers;
- cursor rules and exact ordering; rate limits; idempotency behavior;
- error payload and source-version history; contract test and fixture ID.

Also extract controller/service/serializer/policy cross-references for the most important endpoints. The route list alone does not encode behavior. Treat API docs as the public contract and source as the explanation of edge cases.

### 6.2 Request parsing, pagination and errors

- Support the encodings Mastodon clients actually send: query parameters for reads; URL-encoded/multipart form fields for many writes; JSON where accepted. Parse `media_ids[]`, nested poll fields, repeated arrays, and indexed profile field attributes correctly. A JSON-only API is not compatible.
- Preserve each route’s real method: for example `POST /api/v1/statuses`, `PUT /api/v1/statuses/:id` for edits, `PUT /api/v1/statuses/:id/interaction_policy`, `POST /api/v1/statuses/:id/quotes/:quoting_status_id/revoke`, and `POST /api/v2/media`.
- IDs in JSON are strings, even though core IDs are numeric. Query bounds must compare their numeric values and remain exclusive/inclusive as documented. Generate correct `Link` `next`/`prev` URLs for each paginated endpoint; relation collections may paginate by the relationship row’s ID, not the account’s ID.
- Use Mastodon’s documented error forms, commonly `{ "error": "..." }`, with the actual endpoint status and message. Do not return Pinetree’s Misskey error envelope. Freeze common errors including invalid token/scope, absent/unavailable status, inaccessible reply/quote targets, validation failures, rate limits and unknown routes.
- Add request IDs, body/upload limits, endpoint-specific rate limiting and client IP handling behind explicitly configured trusted proxies. Never log access tokens, auth codes, passwords, private message bodies or signatures with secrets.

### 6.3 OAuth: copy Mastodon behavior, not generic OAuth assumptions

The v4.7.3 [`Doorkeeper configuration`](https://github.com/mastodon/mastodon/blob/v4.7.3/config/initializers/doorkeeper.rb) is a useful source of truth:

- Register apps through `POST /api/v1/apps`; implement authorization-code and client-credentials grants, not the removed resource-owner password grant.
- Support authorization-code flow with **PKCE S256**; verify exact redirect URI matching, state, one-time code use, short-lived authorization codes, allowed scopes, app/client identity and safe redirect handling.
- Match Mastodon’s token-lifetime configuration instead of arbitrarily expiring all tokens or assuming refresh tokens are enabled. In v4.7.3, access tokens have no default expiration; refresh tokens are not enabled by default. Revocation is still required.
- Enforce documented scopes centrally, including broad and granular scopes such as `read`, `write`, `follow`, `push`, `profile`, and endpoint-level `read:statuses`, `write:statuses`, `read:accounts`, `read:notifications`, etc. App-only/client-credentials tokens are not user tokens; endpoint authorization must distinguish them.
- Phase 7.3 adds the `admin:read` and `admin:write` scope families and their per-resource sub-scopes (for example `admin:read:reports`), which the pinned Doorkeeper configuration registers as optional scopes. Birchtree's scope registry lists them (`src/api/oauth/scopes.rs:48-55`). Admin routes also require a moderator-or-admin role (Mastodon's `Admin::PermissionsConcern`). Birchtree has the role model (`src/domain/user.rs:13-20`, `UserRole::Admin`/`Moderator`/`None`) with CLI grants (`src/main.rs:195-196` `--role=`, `src/main.rs:244` `grant-role`; roles decided 2026-10-17, shipped in Phase 7.3). Admin routes are mounted with role-before-scope enforcement (`src/api/mastodon/router.rs:304-322`).
- Do not invent OAuth metadata or userinfo endpoints unless verified in the pinned route/config and needed by the target clients.
- The authorization code flow needs an authenticated browser session and authorization/consent UI. Since this project has no full frontend, render a minimal safe login/consent page on the server; bootstrap the initial local account by CLI. This is protocol support, not an application frontend.

Source: [`OAuth::AuthorizationsController`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/oauth/authorizations_controller.rb), [`Doorkeeper configuration`](https://github.com/mastodon/mastodon/blob/v4.7.3/config/initializers/doorkeeper.rb), [OAuth API docs](https://docs.joinmastodon.org/methods/oauth/), [scope reference](https://docs.joinmastodon.org/api/oauth-scopes/).

### 6.4 Response packing

Create stable DTO packers for Status, Account, Relationship, Notification, Poll, MediaAttachment, ScheduledStatus, Instance and Quote. Preserve per-viewer flags and source/normal modes. Include exact behavior for unavailable accounts, deleted statuses, embedded reblog objects, accepted/pending/rejected/revoked/deleted quotes, custom emoji, edited-at fields, poll totals, and `null` URLs while media is processing. Phase 7.1 changes the media packer: `type` (`gifv` versus `image`), `url`, `preview_url` and `meta` depend on which derived blobs exist, and a derived blob can be pending, which shows as a `null` URL (§5 “Media transcoding”). Avoid one universal packer whose behavior silently changes according to whichever endpoint calls it; share low-level pack functions and pass explicit view context.

## 7. Implementation phases and exit gates

Each phase starts with source/contract review, adds a focused module and tests, records executed evidence in a dated `docs/logs/` entry, and ends with a listed gate. Do not mark a phase complete because routes compile.

**Status 2026-10-20:** the 2026-10-20 fix round is DONE and shipped (commits `372f216` P0 privacy + P1 builds, `f1cda48` P2 confirmed rows, `163ee81`/`f03c0c8` docs + verification evidence; `docs/fix-plan.md` §"Status 2026-10-20"; gate `cargo test --release` 60 suites green + live smoke in `docs/logs/2026-10-20-fix-plan-verification.md`). Phases 0–4, 4.1, 5, 6 (partial: follow lifecycle + content flows shipped, thread-completion + federation tail phased), 7, 7.1, 7.2 and 7.3 are shipped (per-phase notes below). Next: Phase 8 performance qualification.

### Phase 0 — Freeze source contracts and initialize the crate

1. Pin Mastodon tag `v4.7.3`; record its commit SHA, release/version, the API v7 contract (including the quote additions), and reference-client versions.
2. Inventory `config/routes/api.rb` and the public OAuth routes. Build a route table for public/account/status/timeline/media/notification/poll/list/filter/search/preferences/scheduled-status/streaming endpoints; explicitly separate admin/moderation operations and optional/legacy APIs.
3. For each priority endpoint, read controller → service → policy/model → serializer → worker path. Record differences between public API docs and implementation.
4. Freeze first contracts: ID/cursor rules, account/status DTOs, visibility and interaction policies, application ports, durable job envelope, media lifecycle and error mapping.
5. Define a **versioned API compatibility profile**. Report exact scope: local only, federation subset, or full declared public API. Never report full v4.7.3 compatibility by default.

**Exit:** route inventory reviewed against source; domain/DTO glossary and exact test fixtures exist; cargo check/test baseline runs; all unresolved ambiguities are listed explicitly.

### Phase 1 — Runtime, `redb`, IDs, configuration and bootstrap

- Add structured logs, metrics/tracing, health/readiness, graceful shutdown, configuration validation, path permissions, body limits, trusted-proxy handling and release-build CI.
- Implement Mastodon-shaped numeric IDs/decimal-string wire output, `Clock`, ID/time tests and stable sort/cursor semantics.
- Implement versioned `redb` schema, narrow persistence ports, atomic transaction helper patterns, migration runner and common conformance suite.
- Add accounts/local users, password hashing, basic login session for OAuth approval, CLI create-account command, client app registration records, token hashing and durable jobs.
- No public signup/UI beyond what OAuth requires in this phase.

**Exit:** fresh install and restart preserve identity/data; unique-handle races are safe; IDs serialize/cursor correctly; storage conformance and crash-recovery tests prove primary mutation + job atomicity; secret handling tests pass.

### Phase 2 — OAuth, registration and exact read APIs

- Implement `POST /api/v1/apps`, authorization-code/PKCE S256, client credentials, token issue/revoke, session/consent page, scopes and per-route authorization.
- Implement Mastodon-compatible `POST /api/v1/accounts` registration fields and policy gates (open/invite/approval/closed, agreement/rules, email confirmation and confirmation checks where enabled). Add a mail-sender port for verification messages; the sender is an adapter/job, not logic inside the request transaction. The CLI bootstrap remains available even when public registration is closed.
- Implement instance discovery needed by clients (`/api/v1/instance`, `/api/v2/instance`, peers/rules/config fields where in profile), preferences, account lookup, verify/update credentials and account/relationship DTOs. Include quote-policy defaults in `GET /api/v1/preferences` and `source[quote_policy]` in credential updates.
- Implement exact error shapes, pagination headers, request parsing, rate-limit headers and optional-auth behavior before building a large endpoint surface.

**Exit:** a real Mastodon-compatible client can register an app, register an account when policy allows, and obtain a user token; PKCE/replay/redirect/scope/app-token/registration tests pass; account/instance DTO snapshots match the pinned contract.

### Phase 3 — Local statuses, relationships and interaction semantics

Implement vertical slices end-to-end, each through domain → application → persistence → REST contract:

1. Create/show/delete status, source, context, multi-fetch, editing/history, replies and reblogs/undo.
2. Visibility/policy gates, mentions/tags, blocks/domain blocks, follows/follow requests, notification preferences, `show_reblogs`, bookmarks, favourites, pins/endorsements, account notes where in profile.
3. Poll creation/voting/expiry, media attachment relations/order, sensitive/CW/language, application metadata, status counts and per-viewer relationship flags.
4. Quote API and state machine: policy defaults; `quoted_status_id`/`quote_approval_policy`; quote DTO/count; list/revoke; interaction policy endpoint; safe previews; structured/textual fallback. Add local quote authorization before federated quote request handling.
5. Idempotent commands: repeated API retries must not duplicate status, interaction, notification or durable job.
6. Correction (2026-10-17): the domain-block endpoints follow upstream v4.7.3's shape. `GET /api/v1/domain_blocks` pages newest first with numeric `max_id`/`since_id` row-id cursors (limit 100, maximum 200). `POST /api/v1/domain_blocks` and `DELETE /api/v1/domain_blocks` take the `domain` parameter, from the form body or query string. Blank or malformed domains return upstream's `Validation failed` messages. Schema v5 adds the id index and backfills existing rows (executed evidence: `docs/logs/2026-10-17-domain-blocks-correction.md`).

**Exit:** tests cover local flows, privacy boundaries, relationship races, exact DTOs, counters and idempotency; crash after commit still completes follow-up work; a client can perform the core social loop without federation.

### Phase 4 — Media, timelines, notifications and client usability

- Media v1/v2 endpoints, multipart limits, MIME/content validation, processing lifecycle, attachment metadata/description/focus, local file serving, error response/status timing.
- Home, public, local, hashtag, account, list and notification reads with cursor pagination and viewer-aware filtering; correct `Link` headers.
- Notifications and grouping/clear/dismiss/read behavior; preferences and markers as required by the compatibility profile.
- Lists/membership, filters, search, custom emoji and scheduled status operations are their own slices, not one `misc.rs` catch-all.
- Build `StatusViewContext` page loaders and measure DB transaction/query counts as well as wall time.

**Exit:** real client media/post/timeline/notification workflows pass; visibility parity is asserted across every read surface; large-page and sparse-filter tests do not scan without bound; `202`/processing behavior and all documented headers match.

### Phase 4.1 — Parity gap closure (before Phase 5)

Moved up on 2026-10-17. These gaps break the compatibility bar for ordinary clients, and streaming is a larger surface that should not start on top of them. Register entries: D10a, D10b, D11 (`docs/compatibility-profile.md`).

- **JSON request bodies (D11).** Accept `application/json` on every write endpoint, alongside the form encodings. Flatten a JSON body into the key model `src/api/forms.rs` already uses, so every handler sees one shape: nested objects become `a[b]` keys, arrays become repeated `a[]` keys, numbers and booleans become their decimal and `true`/`false` strings, and `null` is absent, as Rails' form encoding renders it. Malformed JSON answers the error body upstream produces; the implementer confirms that body against upstream's parameter-parse handling before writing the test. Form bodies and query strings keep working unchanged.
- **Exclusive lists on home (D10b).** Match upstream's `FeedManager#filter_from_home`. A status from an account that belongs to one of the viewer's exclusive lists is left out of the home feed. Three exact rules: the viewer's own statuses are never left out (upstream returns before the check); list timelines are unaffected (upstream applies the rule only when the timeline type is not `list`); and only lists with `exclusive=true` count.
- **Home follow cap 1000 (D10a).** Raise `HOME_AUTHOR_CAP` from 500 to 1000 (`src/application/timelines.rs`). This is an accepted deviation: an account that follows more than 1000 accounts still misses posts from its older follows. The per-request account cache (`ACCOUNT_CACHE_CAP`, 500 in `src/application/timelines.rs`) also applies to home authors. It must be raised to at least 1000 in the same change, or the extra follows fail closed and their posts are dropped. The Phase 4 bounded-scan limits (`items_scanned <= 5000`, `read_txns <= 600`) must still hold with the new cap. If they do not, the implementer keeps 500 and reports the failing measurement.
- **Tag feed reblogs (D10c): closed, no change.** Upstream's `ReblogService` creates reblog rows with no tag rows, so the tag feed excludes reblog wrappers there too.

**Exit:** every write endpoint has a JSON-and-form parity test with identical results; an exclusive-list member is hidden from home, still visible on the list timeline, and the viewer's own posts still show; the home cap is 1000 and the Phase 4 bounded-scan tests pass; the parity suite (§8 item 11) covers the Phase 0–4.1 surfaces, or the evidence states which surfaces it does not yet cover.

### Phase 5 — Streaming

- Implement SSE and WebSocket server in Rust and Mastodon’s event envelopes: status `update`, `delete`, `status.update`, `notification`, and supported stream-specific events/payloads.
- Implement user/home, user notifications, public/public:local/public:media, hashtag, list and other streams in the pinned profile; obey token types/scopes, parameters, filtering, blocks, mutes and status visibility.
- Support configured streaming hostname discovery/redirect behavior, heartbeats, connection/subscription bounds, cleanup, disconnect/reconnect and backpressure.
- Treat streaming as a consumer of durable state/jobs, not as the durable event log itself. Reconnect semantics must not imply events were persisted for offline clients unless an endpoint specifically provides that behavior.

**Exit:** protocol tests plus actual client smoke tests cover each declared stream; no cross-user/private leakage under concurrent follow/block/unfollow/status-delete changes; stalled clients cannot exhaust memory/tasks.

### Phase 6 — ActivityPub federation

- WebFinger (`acct:` resolution), NodeInfo, actor and key documents, inbox/shared inbox/outbox, object dereferencing, HTTP signatures and digest, signed outbound requests, JSON-LD/ActivityStreams parsing, and canonical URI mapping.
- Initial activity set: Create/Note, Update, Delete, Follow/Accept/Reject/Undo, Like/Undo, Announce/Undo, Block, and quote request/accept/reject/revoke behavior required by Mastodon 4.7.3 interop. Flag send and receive, which reports (Phase 7.3) require.
- Durable per-destination delivery with retry/backoff, failure cooldown, terminal quarantine and replay/dedup. Remote fetches resolve through a safe HTTP client with DNS/IP SSRF defenses, redirect/time/size limits and content validation.
- Inbound activity handling shares local application operations for follow, status, favourite, reblog, delete and quote. Federation failures must not roll back committed local actions.
- Fetch remote media asynchronously; track remote account/status/key cache freshness and safe refresh behavior.
- **Remote thread completion** (`docs/federation-contract.md`, “Remote thread completion”):
  - ingest-time parent resolution: a persisted job per unresolved reply fetches the parent, one hop per job, bounded by a hop cap and a per-trigger fetch budget. It never promotes a reply to root;
  - read-time descendant completion for authenticated `GET /api/v1/statuses/:id/context`: a durable crawl of the remote `replies` collection, announced with `Mastodon-Async-Refresh`, with cooldown, deduplication, and page, item and fetch caps;
  - read-time ancestor completion, for logged-in viewers only (decided 2026-10-17). Mastodon does not do this, so it is deviation D5 in the register (`docs/compatibility-profile.md`). Ancestors and descendants are both completed on the same authenticated context read.
- **Signed fetch.** Outbound dereferences and crawl fetches are signed with the instance actor's key by default. Mastodon's `fetch_resource_without_id_validation` falls back to `Account.representative` when no actor is passed. Birchtree needs an instance-actor keypair before thread completion ships. When authorized fetch is enabled, birchtree's own replies, context and collection endpoints require an HTTP signature (Mastodon: `require_account_signature!` on those controllers).
- Crawl fetches pass the same SSRF, size, time and redirect rules as every other remote fetch. The crawl follows replies on any host, as upstream's all-replies crawl does. The inline reply path's same-host filter (`FetchRepliesService#filter_replies`) does not apply to the crawl. The count caps in `docs/federation-contract.md` bound its cost.

**Exit:** two-instance interop tests pass for both directions, including follow lifecycle, posts, edits, deletes, replies, media, reactions, boosts, quotes and retries; remote thread completion passes its interop cases in `docs/federation-contract.md` (parent chain, hop cap, read-time descendant crawl, authorized fetch); duplicate activity delivery is harmless; SSRF/signature/body-limit security tests pass.

**Status 2026-10-20 (partial):** follow lifecycle + content flows shipped — inbound `Follow/Accept/Reject/Undo/Create/Update/Delete/Like/Announce/Block/Flag/QuoteRequest` dispatch (`src/activitypub/activities.rs:46-57`) into application use cases (`src/application/federation_inbound.rs:100,233-244,272-326,350-462,525-622`), outbound `Create/Update/Delete/Like/Announce/vote` builders (`src/activitypub/outgoing.rs:210-352`), `outbox`/`followers`/`following` collections (`src/activitypub/http.rs:328-447`), per-destination retry/cooldown/quarantine, bounded dedup, SSRF-safe client. Thread completion is implemented (ingest-time one-hop `ResolveParent` + read-time `CrawlReplies`, `src/federation/threads.rs:1-142`, `JobKind` in `src/domain/job.rs:54-56`). Evidence: `docs/logs/2026-10-17-phase-6-verification.md`, `docs/logs/2026-10-17-phase-6-interop-run.out` (Follow family 14/14), `docs/logs/2026-10-18-phase-6-interop-run.out` (content both directions). Tail phased per `docs/fix-plan.md` P3: Move/Add/Remove, relays, remote emoji, note/featured collections, follower sync (D32 refuse-position stands until each lands). §8 item 11 parity suite still not run for federation.

### Phase 7 — Broader 4.7.3 public API parity

Complete the route manifest systematically: remaining account APIs, tag and emoji endpoints, relation collection reads, markers/preferences, directory/collections where part of the target, and the long tail of user-facing endpoints.

In profile, client-facing groups (each route is a row in `docs/endpoint-manifest.md`):

- **Accounts:** `GET /api/v1/accounts?id[]=…`, a multi-fetch of up to 40 ids (`id[]` upstream, `422` above the limit). Unapproved and pending-deletion accounts are omitted. Lookup and search go in the same phase.
- **Conversations:** `GET /api/v1/conversations`, `DELETE /api/v1/conversations/:id`, `POST …/read`, `POST …/unread`, and conversation mutes (`POST /api/v1/statuses/:id/mute|unmute`, `write:mutes`). Remote direct messages wait for Phase 6.
- **Suggestions:** `/api/v1/suggestions` (deprecated upstream since 2021-05-16; the response carries the `Deprecation` header) and `/api/v2/suggestions`. FASP-sourced entries stay out of profile until a FASP integration exists, and are never fabricated.
- **Announcements:** list, dismiss, and reactions. Authoring is through the admin API (`src/api/mastodon/router.rs:201-204`, fix round P1/S2, schema v14 in `src/persistence/redb/migrations.rs:446`), not CLI-only and not HTML-only (upstream authors via HTML; the API authoring surface is new and admin-gated). **Status 2026-10-20:** shipped (`docs/logs/2026-10-20-fix-plan-verification.md`).
- **Donation campaigns:** `GET /api/v1/donation_campaigns` answers honest-absence `404` (P3 operator decision 2026-10-20: the outbound campaign fetch is not built, so no success-shaped `204`; `src/api/mastodon/endpoints/donation_campaigns.rs:8-24`). **Status 2026-10-20:** shipped as honest absence.
- **Web Push:** `/api/v1/push/subscription` (`push` scope). VAPID keys come from configuration. Delivery is a post-commit job (§5), and the push-service HTTP client follows the outbound safety rules.
- **First-party client (keystodon):** `/api/web/*` (embeds, settings, web push subscriptions) is in profile. Upstream guards these routes with the Devise session and CSRF (`protect_from_forgery with: :exception`), not OAuth bearer tokens, and birchtree's plan does the same. Phase 7 adds the prerequisites: a first-party session login (reusing the Phase 2 login session), CSRF tokens for state-changing `/api/web/*` requests, and serving keystodon's built bundle from birchtree (artifact source and mount path fixed when the bundle exists). Proposed default: keystodon's first-party mode uses OAuth bearer tokens for every `/api/v1/*` call, issued to a first-party application, and the session covers login, consent and `/api/web/*` only. That keeps one auth path for the API. Confirm before implementation.
- **Third-party client (keystodon):** OAuth only, against any host. Birchtree's contract for it is §6.3 and Phase 2.

Keep the server-rendered admin dashboard and the Mastodon web app explicitly excluded. Admin API routes move to Phase 7.3. Do not let exclusions imply that ordinary client endpoints are implemented.

Prioritize by actual client usage and contract dependencies, but every skipped route remains an explicit compatibility gap. Repeat endpoint inventory whenever upgrading the target release. Do not silently mix v4.7.3 behavior with `main`.

**Exit:** every endpoint in the Phase 7 client-facing groups has a matching request/response/auth/error test; the target client smoke suite passes (keystodon, once it is available); remaining exclusions are published and obvious. Phases 7.1–7.3 have their own exits.

**Status 2026-10-20:** shipped. Conversations, suggestions (real ranking + member dismiss, schema v13), directory, userinfo, announcements (+ admin authoring), instance sub-resources, push, `/api/web/*`, and reports are mounted (`src/api/mastodon/router.rs:189-206,268-282`); donation campaigns ship as honest-absence 404. Evidence: `docs/logs/2026-10-20-phase-7-loose-verification.md`, `docs/logs/2026-10-20-gaps-round3-verification.md`, `docs/logs/2026-10-20-fix-plan-verification.md`. keystodon is still not on the development machine, so the unmodified-client smoke remains pending (§8 item 11).

### Phase 7.1 — Media transcoding

- Durable transcode job per media attachment that needs derived blobs (§5 "Media transcoding"), with explicit `pending`, `processing`, `ready` and `failed` states.
- **Decided (2026-10-17):**
  - **GIF** (animated or not) → WebM with VP9, typed `video`. `gifv` is never produced (D1, D2).
  - **Video** → WebM with VP9 and Opus audio (D3). **Passthrough:** codec-based, like upstream's `eligible_to_passthrough?` (codecs, never the container): VP9 video with Opus audio or no audio within the upload size limit (`max_file_size`, 16 MB by default) keeps its codecs — byte-identical when already WebM, stream-copied (`-c copy`, no re-encode) into WebM otherwise, so served video bytes are always WebM. Any other video is transcoded to WebM/VP9.
  - **Still images** (JPEG, HEIC, HEIF, PNG, TIFF and others) → WebP (D4). An image already in WebP is passed through unchanged. The `small` preview is WebP too.
  - **Metadata:** the server never strips, rewrites or adds EXIF or any other metadata. Conversions carry the metadata through where the target format can hold it (`ffmpeg -map_metadata 0` is one way). Orientation is not applied to the pixels: the orientation tag is kept and clients apply it. Location metadata therefore stays on the server unless the client removes it before upload. The client does that, by decision.
  - **Stored blobs (proposed default, taken):** a successful transcode replaces the original with the derived blob. The original is deleted once the metadata that points at the derived blob has committed, which keeps it crash-safe under §5. Passthrough keeps the original, which is already the stored blob.
  - **API lifecycle:** `POST /api/v1/media` transcodes inline before it responds, with `200` and the final `url`, within the request time limit. Upstream's v1 also processes inline. `POST /api/v2/media` returns `202` for transcoded types, and the job finishes them: `GET /api/v1/media/:id` answers `206` until the blob is ready and `200` after. This matches today's video behaviour. A transcode that exceeds its time or size limit ends in `failed`, which `GET /api/v1/media/:id` answers with `422`.
  - **Startup validation:** when transcoding is enabled, startup checks that the ffmpeg binary runs, that the VP9 and Opus encoders are present, and that the decoders needed for HEIC, HEIF and TIFF are present. A missing component stops startup. There is no silent pass-through.
  - **Limits:** wall-time, output-size and memory limits per job.
- Open implementation settings, chosen when the phase starts and recorded as configuration defaults: WebP and VP9 quality; the `meta` values for converted blobs (dimensions must match the stored pixels; the orientation rule above applies).

**Exit:** fixture tests for each input class (JPEG, HEIC, PNG, TIFF, GIF, VP9 WebM passthrough, non-VP9 video, VP9 video over the size limit); a metadata test that the EXIF payload survives conversion where the target format holds it; v1 `200` and v2 `202` lifecycle tests; startup-validation tests; crash tests for partial derived blobs.

**Status 2026-10-20:** shipped. The transcode job runs ffmpeg off the async runtime (`src/jobs/worker.rs:107-139`, wiring `src/jobs/transcode.rs:1-12`, implementation `src/media/transcode.rs`); the pipeline toggle is `src/api/mastodon/mod.rs:59`. Evidence: `docs/logs/2026-10-20-gaps-round3-verification.md` (HEIC/TIFF → WebP).

### Phase 7.2 — Search engine

- Status search uses an **embedded** full-text library behind a search backend port. It is not Elasticsearch and not an external service. The library is chosen at scoping time; candidates such as Tantivy are to be evaluated for license, maintenance, and fit with an in-process Rust design.
- Optional, as upstream and Pleroma both allow. Selection is a configuration value with three modes: `embedded` (the library above, **on by default**, decided 2026-10-17; an administrator can switch it off); `database` (the bounded scan that exists today, which is Pleroma's default backend); and `off` (status results are empty, matching Mastodon with Elasticsearch disabled). All three keep the same response contract.
- Index scope follows Pleroma's `indexable?` rule, tightened: only public and unlisted statuses are indexed. Private and direct statuses are never indexed, so they never appear in search results and never leave the database through the index.
- Indexing is a post-commit durable job for create, edit, delete and visibility change, as in Pleroma's `SearchIndexingWorker`. A bounded, restartable backfill builds the index for existing statuses, with a resume cursor as in Pleroma's `Search.Backfill`. The index is derived state, rebuilt from durable records, and never the authoritative store.
- Accounts stay on the database path, as upstream does (`account_search?`). Whether hashtag search uses the engine is checked when the phase is scoped.
- `GET /api/v2/search` keeps its contract (`resolve` behavior, `collections`). `GET /api/v1/search` stays out of profile because it does not exist in v4.7.3.

**Exit:** search parity fixtures for statuses in all three modes and for accounts; index consistency under edit, delete and visibility change; backfill resume and crash tests; the `database` mode matches the Phase 4 interim behavior.

**Status 2026-10-20:** shipped. Three modes behind one response contract (`src/search/mod.rs:3-12`, `SearchMode::Embedded` default in `src/config.rs:94-97,191`); backfill worker `src/jobs/tag_backfill.rs`. Evidence: `docs/logs/2026-10-20-gaps-round3-verification.md`.

### Phase 7.3 — Administration, moderation, trends and analytics

- **Roles (decided 2026-10-17):** two roles on local users, `admin` and `moderator`, granted through CLI commands that extend `create-account`. Proposed permission split, to confirm against upstream's permission set when this phase is scoped: `moderator` covers reports and account actions (the `admin:*:reports` and `admin:*:accounts` routes, read and write); `admin` covers every admin route. Also required: registration approval modes (a Phase 2 gap) for the `approve` and `reject` routes; account suspension, silence and sensitive state; server-level domain allow and block rules; email-domain and canonical email blocks (both need email confirmation, a Phase 2 gap); IP block enforcement in the request path (trusted-proxy client IP, Phase 1); an audit trail for admin actions (proposed default: on, one record per admin write).
- **Admin API** (`/api/v1/admin/*` and `/api/v2/admin/accounts`): accounts, reports, domain allows and blocks, email-domain blocks, IP blocks, canonical email blocks, tags, trend approvals, and measures, dimensions and retention. Upstream uses the `admin:read` and `admin:write` scope families with per-resource sub-scopes. Some POST routes that only read data (`measures`, `dimensions`, `retention`, `canonical_email_blocks/test`) take read scopes upstream, and birchtree keeps that.
- **Reports:** `POST /api/v1/reports` (`write:reports`) and the moderator queue. Remote forwarding (Flag) waits for Phase 6.
- **Trends:** public `GET /api/v1/trends`, `/tags`, `/links` and `/statuses` return only admin-approved items. Upstream paginates them with `offset` and `limit`, and `/api/v1/trends` is a deprecated alias of `/tags`. Link trends need link-preview crawling through the safe client.
- **Annual reports:** generation is a durable job; the reads use `read:accounts` and the generate and read-marking writes use `write:accounts`.

**Exit:** role-gated tests match the upstream forbidden response for a user without the role; scope tests cover each admin family; every admin route has a fixture-backed test or an explicit planned-gap entry; approval-workflow tests for trends; the operator can grant roles through the CLI.

**Status 2026-10-20:** shipped. Roles `admin`/`moderator` with CLI grants (`src/domain/user.rs:13-20`, `src/main.rs:195-196,244`); scope families in the registry (`src/api/oauth/scopes.rs:46-55`); admin accounts/reports/infra/trends routes mounted (`src/api/mastodon/router.rs:304-322` + `admin_infra::routes()`); measures/dimensions/retention moderator-widened (fix round P3). Evidence: `docs/logs/2026-10-20-phase-7-3-verification.md`, `docs/logs/2026-10-20-gaps-round2-verification.md`, `state/logs/2026-10-20-phase7-3-admin-accounts-reports-subagent-b.md`.

### Phase 8 — Performance qualification and optional feed materialization

- Benchmark **release builds**, not only debug builds, with representative account graphs, post counts, remote actors, attachments and client concurrency. Report p50/p95 latency, throughput, memory, read/write contention, index size and background queue delay.
- Include status create/edit/delete, account lookup, page packing, home/public/list/tag timelines, relationships, quote listing, notification reads, media upload, streaming fan-out and federation enqueue/worker delivery. Add transcoding throughput (Phase 7.1), index build and query latency (Phase 7.2), and admin list paging (Phase 7.3).
- Compare current fan-out-on-read against a bounded durable `redb` materialized-feed index using identical result fixtures. Adopt materialization only when measurements justify its write amplification and migration complexity.
- Test process restart, abrupt kill, partial media files, index rebuild, job replay, lease expiry, queue poisoning and high-degree accounts.

**Exit:** published results covering every surface in Phases 0–7.3, with regression thresholds; no unsupported performance claims; the chosen feed strategy meets agreed latency and resource targets on defined hardware.

## 8. Contract and interoperability test matrix

Required suites:

1. **API schema snapshots:** Status, Account, Relationship, Notification, Poll, MediaAttachment, ScheduledStatus, Instance, Quote, quote approval, and source endpoints; assert types, exact optional/null fields, nested objects and decimal-string IDs.
2. **Request parser:** URL-encoded, multipart, JSON where allowed, arrays/nested forms, defaults, repeated params, invalid field handling, Unicode, bounds and content types.
3. **OAuth:** PKCE S256, code expiration/replay, exact redirect match, token revocation, scopes, client-credentials versus user-token behavior, missing/invalid token, private client/secret handling and account suspension.
4. **Policy matrix:** public/unlisted/private/direct/limited across anonymous, author, follower, mentioned user, unrelated user, blocker/blocked, domain-blocked, muted and unavailable account; exercise direct reads, context, timelines, lists, search, quote previews, notifications, SSE/WS and outgoing recipients.
5. **Persistence conformance:** unique indexes, follow/reaction uniqueness, counter consistency, range pagination, transaction atomicity, migrations/backfills, dedup/idempotency, job lease/retry/quarantine and read-after-commit.
6. **Crash tests:** kill before/after commit and during each worker; assert status/relationship + jobs are jointly durable and replay does not duplicate side effects.
7. **Federation fixtures/interop:** Mastodon v4.7.3 fixtures and at least one additional ActivityPub implementation. Test signatures, audience, visibility, actor lookup, unknown reply parents, quote request decisions, redelivery and remote object refresh.
8. **Streaming:** auth, channel/stream names, event envelopes, heartbeats, backpressure, disconnects, access revocation and live policy changes.
9. **Client tests:** automate a real Mastodon-compatible client where practical; keystodon is the planned first target once it is available on this machine, tested in both its first-party mode (session and CSRF on `/api/web/*`, OAuth on `/api/v1/*` under the proposed default) and its third-party mode (OAuth only). Test registration, OAuth, profile, compose/edit/delete, follow, timeline, favourite/boost/bookmark, media, polls, quotes, notifications, streaming, and the Phase 7 groups (conversations, suggestions, announcements, Web Push).
10. **Performance and abuse:** high-degree graph, huge sparse indexes, repeated pagination, large/malformed bodies, upload exhaustion, remote redirects/DNS rebinding, invalid signatures, slow hosts and retry storms.

11. **Unmodified-client parity (the compatibility bar):** run the same client flows against birchtree and against a pinned Mastodon v4.7.3 instance, using unmodified clients. Compare what the client sees: request acceptance, response bodies, status codes, headers the client reads, pagination, streaming events and error text. Score the suite as the share of observations that match upstream. The bar is at least 98% overall, and 100% on the core flows (proposed). Every non-matching observation must be in the deviation register. Run this suite at every phase exit from Phase 3 on, and extend it for each phase.

**Status 2026-10-20:** still not run — no parity harness exists in the tree (no `parity` module under `src/`; the only parity references are inline comments). Every phase exit from 4.1 on records this gap explicitly (`docs/phase-4-1-exit-criteria.md` Parity §, `docs/phase-5-exit-criteria.md:31` with escalation E1). All parity claims to date are source-derived from the pin `8ff11c2b` and protocol-tested against birchtree only.

Use source-level fixtures and docs as a starting point, not the sole oracle. A passing internal mock does not prove client compatibility.

## 9. Non-goals and unresolved risks

- No React, Electron, or frontend source code in this repository. birchtree serves keystodon's built first-party bundle (Phase 7) but does not contain keystodon's source, which is developed in a separate project. No server-rendered admin dashboard (`config/routes/admin.rb`): operators use the admin API (Phase 7.3) and the CLI.
- No PostgreSQL, Redis, or separate broker. If later measurements prove a hard limitation, revisit the design as an explicit decision rather than quietly adding infrastructure.
- No multi-node mode until its storage adapter, media arrangement, ownership/routing and the shared conformance suite are implemented and tested.
- No promise that a single-node `redb` setup will match Mastodon’s high-degree home-feed performance. Mastodon’s Redis-backed home/list feed materialization is a major architectural difference that requires measurement.
- No blind copying of the Rails data model, cache keys, callbacks or Sidekiq job names. Translate externally visible outcomes into Rust contracts.
- Admin API compatibility is in profile (Phase 7.3), and it has shipped with its prerequisites: the role model (`src/domain/user.rs:13-20`), CLI role grants (`src/main.rs:195-196,244`), registration approval modes, and the enforcement points listed in Phase 7.3. Admin routes are mounted with role-before-scope enforcement (`src/api/mastodon/router.rs:304-322`), never success-shaped. Registration policy and moderation keep operator controls (CLI and configuration), and the admin API does not imply a web admin UI.

Open decisions, each to be resolved in the phase named rather than left implicit: supported signup/verification modes (Phase 2 gaps; needed by the Phase 7.3 approval routes); exact target clients (keystodon, in both modes, once it is available on this machine; client smoke tests wait for it); whether collections are in the declared profile (push is in, Phase 7); the embedded search library (Phase 7.2); the image target format and the other Phase 7.1 sub-decisions; keystodon's bundle mount path and artifact source, and whether first-party `/api/v1/*` calls use OAuth (proposed) or the session (Phase 7); the admin permission split and audit trail (Phase 7.3; proposed defaults in Phase 7.3);  maximum status/media limits; local-only or closed-registration default; whether public web/ActivityPub object routes are part of the first federation milestone (Phase 6); and latency/resource acceptance thresholds (Phase 8). Decided 2026-10-17 and no longer open: read-time ancestor completion for logged-in viewers (deviation D5); admin and moderator roles; the domain-block correction; search on by default with an administrator switch (D6); JSON request bodies moved up to Phase 4.1; the 98% bar; video as VP9 WebM with passthrough, GIF as VP9 WebM, and still images as WebP (D2–D4); the home follow cap at 1000 (D10a); exclusive lists matching upstream (D10b); no `206` for the home feed (D9).

## 10. Source files and current public contracts reviewed

Pinned Mastodon v4.7.3 sources reviewed for this revision:

- [API route inventory](https://github.com/mastodon/mastodon/blob/v4.7.3/config/routes/api.rb)
- [Status controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/statuses_controller.rb)
- [Post status service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/post_status_service.rb)
- [Update status service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/update_status_service.rb)
- [Status model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/status.rb), [visibility concern](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/concerns/status/visibility.rb), [status policy](https://github.com/mastodon/mastodon/blob/v4.7.3/app/policies/status_policy.rb)
- [Status serializer](https://github.com/mastodon/mastodon/blob/v4.7.3/app/serializers/rest/status_serializer.rb), [account serializer](https://github.com/mastodon/mastodon/blob/v4.7.3/app/serializers/rest/account_serializer.rb)
- [Quote model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/quote.rb), [quote request activity](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/activity/quote_request.rb), [interaction policy concern](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/concerns/status/interaction_policy_concern.rb)
- [Account model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/account.rb), [Follow model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/follow.rb), [Notification model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/notification.rb), [database schema](https://github.com/mastodon/mastodon/blob/v4.7.3/db/schema.rb), [Snowflake ID generator](https://github.com/mastodon/mastodon/blob/v4.7.3/lib/mastodon/snowflake.rb)
- [Feed manager](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/feed_manager.rb), [home timeline controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/timelines/home_controller.rb), [streaming redirect controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/streaming_controller.rb)
- [ActivityPub activity dispatcher](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/activity.rb), [remote status parser](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/parser/status_parser.rb), [delivery worker](https://github.com/mastodon/mastodon/blob/v4.7.3/app/workers/activitypub/delivery_worker.rb)
- [Doorkeeper configuration](https://github.com/mastodon/mastodon/blob/v4.7.3/config/initializers/doorkeeper.rb), [media v1 endpoint](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/media_controller.rb), [media v2 endpoint](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v2/media_controller.rb)

Pinned sources added by the 2026-10-17 re-scope (Phase 6 and Phases 7–7.3):

- Routes and controllers: [admin API](https://github.com/mastodon/mastodon/tree/v4.7.3/app/controllers/api/v1/admin), [v2 admin](https://github.com/mastodon/mastodon/tree/v4.7.3/app/controllers/api/v2/admin), [admin permissions concern](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/concerns/admin/permissions_concern.rb), [reports](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/reports_controller.rb), [trends](https://github.com/mastodon/mastodon/tree/v4.7.3/app/controllers/api/v1/trends), [push subscriptions](https://github.com/mastodon/mastodon/tree/v4.7.3/app/controllers/api/v1/push), [web routes](https://github.com/mastodon/mastodon/tree/v4.7.3/app/controllers/api/web), [conversations](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/conversations_controller.rb), [suggestions](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v2/suggestions_controller.rb), [announcements](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/announcements_controller.rb), [donation campaigns](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/donation_campaigns_controller.rb)
- Remote threads: [context controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/statuses/contexts_controller.rb), [thread resolve worker](https://github.com/mastodon/mastodon/blob/v4.7.3/app/workers/thread_resolve_worker.rb), [fetch-all-replies worker](https://github.com/mastodon/mastodon/blob/v4.7.3/app/workers/activitypub/fetch_all_replies_worker.rb), [fetch-all-replies service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/activitypub/fetch_all_replies_service.rb), [fetch-replies service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/activitypub/fetch_replies_service.rb), [fetch-replies concern](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/concerns/status/fetch_replies_concern.rb), [Create activity](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/activitypub/activity/create.rb), [remote status service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/activitypub/fetch_remote_status_service.rb), [JSON-LD helper](https://github.com/mastodon/mastodon/blob/v4.7.3/app/helpers/json_ld_helper.rb)
- Search, media and feeds: [search service](https://github.com/mastodon/mastodon/blob/v4.7.3/app/services/search_service.rb), [GIF transcoder](https://github.com/mastodon/mastodon/blob/v4.7.3/lib/paperclip/gif_transcoder.rb), [media attachment model](https://github.com/mastodon/mastodon/blob/v4.7.3/app/models/media_attachment.rb), [feed manager](https://github.com/mastodon/mastodon/blob/v4.7.3/app/lib/feed_manager.rb), [domain blocks controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/domain_blocks_controller.rb), [accounts controller](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/api/v1/accounts_controller.rb)
- Optional-search reference (a design reference, not the compatibility authority): Pleroma `lib/pleroma/search/` (`search_backend.ex`, `database_search.ex`, `meilisearch.ex`, `parade_db.ex`, `qdrant_search.ex`, `backfill.ex`) and `lib/pleroma/search.ex`, from the reference checkout `~/fedifedifedi/pleroma` (commit `687fa6e1b`, `develop`).

Public docs used to verify wire contracts:

- [OAuth](https://docs.joinmastodon.org/methods/oauth/) · [OAuth scopes](https://docs.joinmastodon.org/api/oauth-scopes/)
- [Statuses, including quote API](https://docs.joinmastodon.org/methods/statuses/) · [quote client guide](https://docs.joinmastodon.org/client/quotes/)
- [Streaming](https://docs.joinmastodon.org/methods/streaming/) · [accounts](https://docs.joinmastodon.org/methods/accounts/) · [instance](https://docs.joinmastodon.org/methods/instance/)
- [media](https://docs.joinmastodon.org/methods/media/) · [timelines](https://docs.joinmastodon.org/methods/timelines/) · [ActivityPub interoperability](https://docs.joinmastodon.org/spec/activitypub/)

Pinetree references that inform engineering patterns, not Mastodon behavior:

- [Pinetree architecture proposal](https://github.com/estyseesghosts/pinetree/blob/master/sns-server-proposal.md)
- [Pinetree agent rules](https://github.com/estyseesghosts/pinetree/blob/master/AGENTS.md)
- [Pinetree application ports](https://github.com/estyseesghosts/pinetree/blob/master/docs/application-ports.md)
- [Pinetree persistence conformance](https://github.com/estyseesghosts/pinetree/blob/master/docs/persistence-conformance.md)
- [Pinetree quote implementation guide](https://github.com/estyseesghosts/pinetree/blob/master/docs/quote-posts.md) · [branched replies guide](https://github.com/estyseesghosts/pinetree/blob/master/docs/replies.md)

