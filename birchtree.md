# Rust Mastodon-Compatible Server — Revised Implementation Plan

**Revised:** 2026-10-16  
**Reference release:** Mastodon **4.7.3** (released 2026-10-01)  
**Compatibility goal:** a progressively tested drop-in replacement for Mastodon clients, targeting the v4.7.3 / API v7 contract; not a claim of complete API parity on day one  
**Initial deployment:** one Rust process, `redb`, local media storage, in-process durable-job workers  
**Frontend:** no application UI. A minimal server-rendered login/consent flow is required for Mastodon-compatible OAuth authorization.

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

### What “drop-in” means here

“Drop-in” is an end state, not the first milestone. Define the intended client profile and test it against actual Mastodon-compatible clients. A server that implements posting and timelines but lacks scope semantics, media processing, account registration, streaming, or federation is **not** a drop-in replacement.

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
- A remote reply with an unknown parent stays unresolved; never silently promote it to a root. Persist bounded parent-resolution jobs and withhold it from ordinary reads until linking succeeds, unless the pinned Mastodon behavior demands a different explicit projection.
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
- Do not invent OAuth metadata or userinfo endpoints unless verified in the pinned route/config and needed by the target clients.
- The authorization code flow needs an authenticated browser session and authorization/consent UI. Since this project has no full frontend, render a minimal safe login/consent page on the server; bootstrap the initial local account by CLI. This is protocol support, not an application frontend.

Source: [`OAuth::AuthorizationsController`](https://github.com/mastodon/mastodon/blob/v4.7.3/app/controllers/oauth/authorizations_controller.rb), [`Doorkeeper configuration`](https://github.com/mastodon/mastodon/blob/v4.7.3/config/initializers/doorkeeper.rb), [OAuth API docs](https://docs.joinmastodon.org/methods/oauth/), [scope reference](https://docs.joinmastodon.org/api/oauth-scopes/).

### 6.4 Response packing

Create stable DTO packers for Status, Account, Relationship, Notification, Poll, MediaAttachment, ScheduledStatus, Instance and Quote. Preserve per-viewer flags and source/normal modes. Include exact behavior for unavailable accounts, deleted statuses, embedded reblog objects, accepted/pending/rejected/revoked/deleted quotes, custom emoji, edited-at fields, poll totals, and `null` URLs while media is processing. Avoid one universal packer whose behavior silently changes according to whichever endpoint calls it; share low-level pack functions and pass explicit view context.

## 7. Implementation phases and exit gates

Each phase starts with source/contract review, adds a focused module and tests, records executed evidence in a dated `docs/logs/` entry, and ends with a listed gate. Do not mark a phase complete because routes compile.

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

**Exit:** tests cover local flows, privacy boundaries, relationship races, exact DTOs, counters and idempotency; crash after commit still completes follow-up work; a client can perform the core social loop without federation.

### Phase 4 — Media, timelines, notifications and client usability

- Media v1/v2 endpoints, multipart limits, MIME/content validation, processing lifecycle, attachment metadata/description/focus, local file serving, error response/status timing.
- Home, public, local, hashtag, account, list and notification reads with cursor pagination and viewer-aware filtering; correct `Link` headers.
- Notifications and grouping/clear/dismiss/read behavior; preferences and markers as required by the compatibility profile.
- Lists/membership, filters, search, custom emoji and scheduled status operations are their own slices, not one `misc.rs` catch-all.
- Build `StatusViewContext` page loaders and measure DB transaction/query counts as well as wall time.

**Exit:** real client media/post/timeline/notification workflows pass; visibility parity is asserted across every read surface; large-page and sparse-filter tests do not scan without bound; `202`/processing behavior and all documented headers match.

### Phase 5 — Streaming

- Implement SSE and WebSocket server in Rust and Mastodon’s event envelopes: status `update`, `delete`, `status.update`, `notification`, and supported stream-specific events/payloads.
- Implement user/home, user notifications, public/public:local/public:media, hashtag, list and other streams in the pinned profile; obey token types/scopes, parameters, filtering, blocks, mutes and status visibility.
- Support configured streaming hostname discovery/redirect behavior, heartbeats, connection/subscription bounds, cleanup, disconnect/reconnect and backpressure.
- Treat streaming as a consumer of durable state/jobs, not as the durable event log itself. Reconnect semantics must not imply events were persisted for offline clients unless an endpoint specifically provides that behavior.

**Exit:** protocol tests plus actual client smoke tests cover each declared stream; no cross-user/private leakage under concurrent follow/block/unfollow/status-delete changes; stalled clients cannot exhaust memory/tasks.

### Phase 6 — ActivityPub federation

- WebFinger (`acct:` resolution), NodeInfo, actor and key documents, inbox/shared inbox/outbox, object dereferencing, HTTP signatures and digest, signed outbound requests, JSON-LD/ActivityStreams parsing, and canonical URI mapping.
- Initial activity set: Create/Note, Update, Delete, Follow/Accept/Reject/Undo, Like/Undo, Announce/Undo, Block, and quote request/accept/reject/revoke behavior required by Mastodon 4.7.3 interop. Add `Flag`/report behavior if the selected profile includes it.
- Durable per-destination delivery with retry/backoff, failure cooldown, terminal quarantine and replay/dedup. Remote fetches resolve through a safe HTTP client with DNS/IP SSRF defenses, redirect/time/size limits and content validation.
- Inbound activity handling shares local application operations for follow, status, favourite, reblog, delete and quote. Federation failures must not roll back committed local actions.
- Fetch remote media asynchronously; track remote account/status/key cache freshness and safe refresh behavior.

**Exit:** two-instance interop tests pass for both directions, including follow lifecycle, posts, edits, deletes, replies, media, reactions, boosts, quotes and retries; duplicate activity delivery is harmless; SSRF/signature/body-limit security tests pass.

### Phase 7 — Broader 4.7.3 public API parity

Complete the route manifest systematically: remaining account APIs, polls, search, lists, filters, tag/emoji/trending and suggestion endpoints, markers/preferences, scheduled status operations, push notifications if in scope, directory/collections where part of the target, and the long tail of user-facing endpoints. Keep admin API, moderation dashboard, and full Mastodon web UI explicitly excluded until separately scoped; do not let exclusions imply that ordinary client endpoints are implemented.

Prioritize by actual client usage and contract dependencies, but every skipped route remains an explicit compatibility gap. Repeat endpoint inventory whenever upgrading the target release. Do not silently mix v4.7.3 behavior with `main`.

**Exit:** every endpoint in the declared compatibility profile has a matching request/response/auth/error test; target client smoke suite passes; remaining exclusions are published and obvious.

### Phase 8 — Performance qualification and optional feed materialization

- Benchmark **release builds**, not only debug builds, with representative account graphs, post counts, remote actors, attachments and client concurrency. Report p50/p95 latency, throughput, memory, read/write contention, index size and background queue delay.
- Include status create/edit/delete, account lookup, page packing, home/public/list/tag timelines, relationships, quote listing, notification reads, media upload, streaming fan-out and federation enqueue/worker delivery.
- Compare current fan-out-on-read against a bounded durable `redb` materialized-feed index using identical result fixtures. Adopt materialization only when measurements justify its write amplification and migration complexity.
- Test process restart, abrupt kill, partial media files, index rebuild, job replay, lease expiry, queue poisoning and high-degree accounts.

**Exit:** published results and regression thresholds; no unsupported performance claims; the chosen feed strategy meets agreed latency and resource targets on defined hardware.

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
9. **Client tests:** automate a real Mastodon-compatible client where practical; test registration, OAuth, profile, compose/edit/delete, follow, timeline, favourite/boost/bookmark, media, polls, quotes, notifications and streaming.
10. **Performance and abuse:** high-degree graph, huge sparse indexes, repeated pagination, large/malformed bodies, upload exhaustion, remote redirects/DNS rebinding, invalid signatures, slow hosts and retry storms.

Use source-level fixtures and docs as a starting point, not the sole oracle. A passing internal mock does not prove client compatibility.

## 9. Non-goals and unresolved risks

- No React, Electron, separate client app, full web UI, or admin dashboard in the initial plan.
- No PostgreSQL, Redis, or separate broker. If later measurements prove a hard limitation, revisit the design as an explicit decision rather than quietly adding infrastructure.
- No multi-node mode until its storage adapter, media arrangement, ownership/routing and the shared conformance suite are implemented and tested.
- No promise that a single-node `redb` setup will match Mastodon’s high-degree home-feed performance. Mastodon’s Redis-backed home/list feed materialization is a major architectural difference that requires measurement.
- No blind copying of the Rails data model, cache keys, callbacks or Sidekiq job names. Translate externally visible outcomes into Rust contracts.
- No full administrative compatibility in the public-client milestone. Moderation/security operations and registration policies must still have operator controls even without a web admin UI.

Open decisions to resolve in Phase 0 rather than leave implicit: supported signup/verification modes; exact target clients; whether push notifications and collections are in the declared profile; policy for search indexing without Elasticsearch; maximum status/media limits; local-only or closed-registration default; whether public web/ActivityPub object routes are part of the first federation milestone; and latency/resource acceptance thresholds.

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

