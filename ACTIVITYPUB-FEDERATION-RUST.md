# `activitypub-federation` Rust: implementation guide for a Mastodon replacement

**Purpose:** Guide coding agents building a Rust server that aims to be a drop-in replacement for Mastodon while using [`activitypub-federation`](https://github.com/LemmyNet/activitypub-federation-rust) for ActivityPub server-to-server federation.

**Research checked:** 2026-10-16  
**Compatibility baseline:** Mastodon **v4.7.3**. Treat the tag as the behavioral contract; use `main` only to identify newer behavior that is intentionally out of scope.

## 1. Executive summary

`activitypub-federation` is a useful **federation library**, not a Mastodon server framework. It provides abstractions for ActivityPub actors and objects, remote dereferencing, inbox processing, HTTP-signature handling, WebFinger utilities, and outbound activity delivery. The application defines its own data structures, persistence, domain rules, endpoint routing, and protocol-specific behavior.

Use the crate behind a narrow federation adapter. Do not make its traits, serialized structs, or fetch cache the server's complete domain model. The replacement must separately implement Mastodon's REST API, OAuth behavior, streaming API, timelines, moderation, media, notifications, and Mastodon-specific federation semantics.

The library intentionally leaves sanitization and application-level validation to its caller. A successfully verified signature means the request was authenticated under the relevant signature scheme; it does **not** authorize every action described by the activity.

### Responsibility boundary

| Area | Provided by the crate | Still owned by this server |
|---|---|---|
| ActivityPub wire data | Helpers and traits for defining/serializing application-owned protocol types | Mastodon-compatible actor, Note, Question, activity and extension formats |
| Actors and objects | `Actor`, `Object`, `ObjectId<T>`, `Collection` abstractions | Canonical domain records, IDs, relationships, persistence and migration policy |
| Remote discovery | WebFinger resolution helpers and object dereferencing | Local WebFinger endpoint, account resolution policy, cache policy and errors exposed to API clients |
| Inbox | HTTP integration, body/signature checks and `ActivityHandler` dispatch | Correct route setup, activity-specific validation, authorization, deduplication and domain effects |
| Outbound federation | Signed delivery helpers, recipient-inbox deduplication, local-domain exclusion and in-process retries | Recipient calculation, durable outbox, visibility rules, delivery observability and restart-safe retry state |
| Security | Useful protocol checks and URL-verification hooks | Defense in depth, safe HTML/media handling, semantic authorization, abuse controls and SSRF policy verification |
| Mastodon client API | No general Mastodon REST compatibility layer | Routes, schemas, OAuth scopes, status/account semantics, pagination, errors, streaming and all client-facing behavior |
| Runtime/storage | Configuration hooks for application state and HTTP framework integration | redb schema/indexes, transactions, background jobs, caches and performance design |

Primary references: [crate docs](https://docs.rs/activitypub_federation/latest/activitypub_federation/), [crate source](https://github.com/LemmyNet/activitypub-federation-rust), [Mastodon federation specification](https://github.com/mastodon/mastodon/blob/v4.7.3/FEDERATION.md), [Mastodon API docs](https://docs.joinmastodon.org/).

## 2. Version, framework and licensing decisions

As checked on 2026-10-16:

- The latest stable crate release is **0.6.6**. Its release notes fix two serious Axum inbox issues: missing request-body hash validation and unbounded request-body reads. The release says the signature/body issue affected the Axum integration, not the Actix integration. Do not use an older version for a new server.
- The repository's `main` manifest declares **0.7.0-beta.12**, a prerelease. It uses Axum 0.8.8; stable 0.6.6 uses Axum 0.7.5. Those framework types are not interchangeable. Choose a crate/framework pairing intentionally and pin the exact version; do not follow `main` or prereleases implicitly.
- The crate's Cargo features default to both `actix-web` and `axum`. Disable default features and enable only the framework integration actually used, unless there is a tested reason to do otherwise.
- The crate is **AGPL-3.0** licensed. Review the license and distribution/deployment implications before adopting it; do not treat it as MIT/Apache-licensed infrastructure.

Suggested decision rule:

1. If the server is on Axum 0.7, evaluate stable `0.6.6` first.
2. If the server is on Axum 0.8, evaluate and pin `0.7.0-beta.12` (or a newer explicitly reviewed release) rather than forcing incompatible Axum versions into one router. Record that the chosen crate is prerelease and run the compatibility/security suite before upgrades.
3. If using Actix, make the same version and security review; do not infer that an issue affecting one framework adapter affects the other identically.

Example feature selection (adjust the version and feature names to the chosen release):

```toml
activitypub_federation = {
  version = "=0.6.6",
  default-features = false,
  features = ["axum"],
}
```

This example is for an Axum 0.7 project only. For Axum 0.8, select the exact reviewed beta instead. Do not copy the example blindly into a project whose framework version differs.

### Security-version notes

Two crate advisories matter when selecting a version:

- **CVE-2026-33693 / GHSA-q537-8fr5-cw35:** an SSRF bypass involving `0.0.0.0`; the advisory identifies `0.7.0-beta.9` as fixed in the prerelease series. Its semver range and enumerated affected-version list are not perfectly aligned for releases published later, so agents must check the current advisory and resolved crate source rather than infer safety from a version string alone.
- **Stable 0.6.6 release note:** adds Axum inbox request-body hash verification and a body-size limit, fixing an authentication/integrity problem and potential denial of service in prior Axum versions.

Before finalizing a dependency update, run `cargo audit` (or the project's equivalent), inspect the resolved `Cargo.lock`, review the exact selected release's changelog/source and add regression tests for the known issue classes. Never enable the crate's debug mode or permissive local-URL behavior in production.

Sources: [0.6.6 release notes](https://github.com/LemmyNet/activitypub-federation-rust/releases/tag/0.6.6), [SSRF advisory](https://github.com/advisories/GHSA-q537-8fr5-cw35), [crate manifest on main](https://github.com/LemmyNet/activitypub-federation-rust/blob/main/Cargo.toml), [stable release docs](https://docs.rs/crate/activitypub_federation/0.6.6).

## 3. What the main abstractions mean

Names and signatures can change between 0.6.x and 0.7 beta. Check the docs and source for the pinned release before implementing these traits.

### `FederationConfig`

The crate's shared configuration establishes the local domain and exposes application state to handlers. It also centralizes federation settings such as URL verification, HTTP behavior and debugging options. Keep redb handles, domain services and runtime configuration in the application state supplied to the crate, rather than making the crate the owner of those systems.

Local actor and object URLs should be stable and HTTPS-accessible in production. Debug options for localhost development must never leak into production configuration.

### `Object`, `Actor`, `ObjectId<T>` and `Collection`

- **`Object`** connects an application-owned database type to its serialized ActivityPub representation, including the conversion/fetch behavior required by the crate.
- **`Actor`** adds actor-specific federation behavior, including inbox/key information and other actor endpoints expected by the selected crate version.
- **`ObjectId<T>`** wraps an ActivityPub URL with an expected Rust object type and supports resolving remote objects into the application's local representation.
- **`Collection`** helps represent/federate collections; it does not create or maintain Mastodon's followers, lists or timeline indexes for you.

The crate deliberately does not prescribe complete data structures for federated applications. It uses application-defined Rust types and Serde rather than requiring a general-purpose JSON-LD processor. This is a practical wire-format strategy, not a guarantee that every JSON-LD document shape or extension is supported automatically.

**Design rule:** keep three separate representations:

1. **Domain/storage model:** canonical account, status, relationship, media, notification and moderation records.
2. **ActivityPub wire model:** serialized actors, objects and activities used for federation.
3. **Mastodon REST model:** API response/request shapes, such as `Account`, `Status`, `Notification`, and error objects.

Conversions between them should be explicit. Never expose storage structs as REST responses or use inbound JSON as the storage schema directly.

### `ActivityHandler` and `receive_activity`

An incoming activity type implements the crate's handler contract, including an activity ID, actor identity, a verification step and a receive step. An inbox route passes the request to the crate's framework helper, which performs the crate's protocol checks and dispatches to the typed handler.

This dispatch mechanism does not implement Mastodon's semantics. The application's handler must decide whether an activity is supported, authorized and valid for the current domain state, then call a domain service to apply it. A handler that returns success from `verify` without checks is only a demonstration, not production policy.

Use narrow activity handlers that invoke reusable services. Do not put parsing, authorization, database mutations, notification fan-out and network delivery in one `receive` method.

### Fetching and WebFinger

Remote IDs can be dereferenced and WebFinger helpers can resolve a remote handle to an actor. Remote fetches are I/O and may fail, time out or return malicious data; they must not be treated as ordinary local database reads.

Expose local WebFinger and ActivityPub actor/object routes yourself. Ensure WebFinger maps local `acct:` identities to the canonical local actor URL and follows Mastodon's documented behavior. Serve ActivityPub JSON for local actors and objects with the appropriate ActivityPub media type/JSON-LD context, while leaving human-facing HTML behavior separate.

### Sending activities

The crate can prepare and sign outbound activities, resolve recipient inboxes, deduplicate shared inboxes and avoid sending back to the local domain. Its built-in queue retries delivery failures, but the retry queue is in memory and is lost after a process crash/restart. The crate's own documentation identifies a custom queue as the route to persistent retries.

Therefore use the crate to perform federation delivery, but make the server own the durable delivery record and retry policy. Never rely on an in-memory retry queue for delivery guarantees.

## 4. Recommended application architecture

Keep federation as an adapter around the domain layer.

```text
Mastodon-compatible HTTP surface
  ├── REST API and serializers
  ├── OAuth and scope enforcement
  └── Streaming API
             │
             ▼
Domain services
  ├── accounts / follows / blocks
  ├── statuses / edits / replies / boosts / quotes
  ├── visibility / mentions / notifications
  └── moderation / polls / media metadata
             │
       ┌─────┴─────────────┐
       ▼                   ▼
redb and indexes     Durable job/outbox records
                           │
                           ▼
              ActivityPub adapter
              ├── activitypub-federation
              ├── wire DTOs and conversions
              ├── inbox handlers
              └── outbound serialization
                           │
                           ▼
                   Remote Fediverse
```

### Outgoing flow

1. Validate the REST request and authorization in the API layer.
2. Apply the domain operation in a redb transaction.
3. Within the same commit boundary, persist the ActivityPub activity/outbox event and intended delivery work. If the storage design cannot atomically commit all records, define and test a recoverable consistency strategy.
4. A worker claims durable delivery work and calls the crate to sign/send the activity.
5. Record success or failure, attempt count, last error and next retry time. Retry transient failures; classify permanent failures; expose metrics/logs for operators.

A successful local post must not depend on all remote servers being online. Conversely, a local transaction must not commit a status while silently losing the activity required to federate it.

### Incoming flow

1. Apply request limits at the HTTP boundary and pass the complete signed request to the correct crate inbox integration.
2. Let the crate perform the selected version's HTTP body/signature checks and typed activity parsing.
3. Apply application-level checks: authenticated actor versus activity actor, object ownership, relationship state, permissions, visibility and supported activity semantics.
4. Deduplicate by remote activity ID and perform a single idempotent domain operation.
5. Commit local effects and durable follow-up jobs; do not wait for outbound HTTP delivery while holding a redb write transaction.

Do not assume the crate's activity cache is a durable idempotency ledger. Use persistent IDs and domain-level uniqueness constraints/logic.

## 5. Mastodon compatibility is a separate project surface

ActivityPub does not define Mastodon's client API. Mastodon's REST API is an application-specific contract, and ActivityPub Client-to-Server support is not the mechanism used by mainstream Fediverse clients. A drop-in implementation must reproduce the documented API behaviors, not merely accept and emit valid ActivityPub.

Use the exact [Mastodon v4.7.3 source tag](https://github.com/mastodon/mastodon/tree/v4.7.3) and [API documentation](https://docs.joinmastodon.org/) as references. Where documentation and source disagree, add a compatibility test and record which behavior is intentionally followed.

At minimum, separately implement and test:

- **REST API:** routes, query/body parameters, response fields, status codes, validation errors, pagination and rate limits.
- **OAuth:** application registration, authorization code flow/PKCE where applicable, token issuance/revocation, client authentication and scopes. Don't substitute generic OAuth defaults for Mastodon's observed behavior.
- **Core social model:** accounts, profiles, follows, follow requests, blocks, mutes, lists, statuses, replies, boosts, favourites, bookmarks and pins.
- **Visibility and authorization:** public, unlisted, followers-only and direct/mention-only posts; blocked/muted actors; protected accounts; quote restrictions and private-content access.
- **Timeline and notification behavior:** home, local, federated, list, hashtag and public feeds; notification persistence, filtering and unread state.
- **Streaming:** endpoint discovery, authentication, event names/payloads, heartbeats, disconnect/reconnect behavior and authorization. Mastodon documents SSE and WebSocket contracts separately.
- **Media and content:** attachment upload/processing, remote media, alt text, content warnings, custom emoji, link cards, HTML sanitization, polls, scheduled posts and edit history.
- **Instance behavior:** instance metadata/configuration, discovery, search, moderation/reporting, admin APIs and any endpoints clients use during onboarding.

Do not call the implementation a drop-in replacement until tests cover a stated compatibility matrix of endpoints and behaviors. An API can compile and return plausible JSON while still breaking real clients through missing fields, subtle OAuth differences, ordering, pagination or error semantics.

## 6. Mastodon-specific federation details to audit

Mastodon follows ActivityPub but has established conventions and extensions. The crate will not automatically reproduce them.

### Required discovery and addressing

- **WebFinger:** a local account handle must resolve to the canonical ActivityPub actor URI; remote handle lookup must handle discovery failures and ambiguous/invalid results safely.
- **Actor and object routes:** local actors and objects must be dereferenceable in ActivityPub JSON form; actor metadata must contain consistent IDs, inboxes, public keys and profile fields.
- **Audience and mentions:** interpret `to`, `cc`, Public, follower collections and Mention tags consistently. Mastodon uses audience addresses and mention tags together to infer visibility and generate mention notifications. Do not infer visibility from one property alone.
- **Shared inboxes:** deliver only to the intended recipients, deduplicate shared inbox URLs and prevent direct/private messages from being exposed via a public/shared-inbox path.

### Activity/object coverage

Start with the activities and objects in the selected Mastodon source, not an enum chosen by convenience. Current Mastodon federation documentation identifies `Note` and `Question` as first-class object types and documents activity behavior including `Create`, `Update`, `Delete`, `Like`, `Announce`, `Undo`, `Flag`, and quote-related behavior. Add other types, including profile changes, follow negotiation, blocks, moves, polls, and collection operations, according to the actual compatibility target.

Important cases include:

- `Create` for a post and `Question` for a poll;
- `Update` for edited statuses, profile changes and poll vote-count updates where applicable;
- `Delete` and `Undo` for removal and reversal of interactions;
- `Follow`/`Accept`/`Reject` and follow-request state;
- `Like`/`Undo` and `Announce`/`Undo`;
- blocks, reports (`Flag`), account moves and featured/collection operations;
- content warnings, sensitive media, mentions, hashtags, custom emoji, reply references and attachment variants;
- quote posts, including consent/authorization and legacy representations.

Do not treat receiving an activity as proof that its referenced object is complete, authorized or accessible. Remote formats vary. Normalize incoming representations into one internal model while retaining enough canonical source information to update and reserialize them correctly.

### HTTP signature and key compatibility: a required spike

Mastodon v4.7 supports legacy draft-Cavage HTTP Signatures and newer HTTP Message Signatures (RFC 9421) under documented conditions; its v4.7 federation documentation also describes Ed25519 key verification and FEP-521a key representations. The crate's dependency manifest shows the `http-signature-normalization` stack and an RSA dependency, but that alone does **not** prove compatibility with every signature scheme/key type Mastodon accepts.

Before relying on the crate as the complete signature layer, inspect the selected release's implementation and write integration tests against Mastodon v4.7.3 for:

1. legacy RSA signature verification and outbound signing;
2. RFC 9421 requests and its required headers/canonicalization;
3. Ed25519 public-key representations and signature verification;
4. actor key rotation, missing keys, stale caches and key-fetch failures;
5. signed GET behavior where required by the remote instance's security configuration;
6. request digest/body integrity and rejection of body substitution.

If a scheme is not supported, implement a deliberately isolated compatibility adapter or contribute the missing support upstream. Do not weaken verification to make an interoperability test pass. Keep signing algorithm/key selection explicit and test key ownership against the actor URI.

### Quote posts

Mastodon v4.7 documents FEP-044f consent-respecting quote posts and has client API controls for quoted statuses and quote policy. Incoming servers may send different quote representations depending on implementation/version. Normalize quotes into one internal representation, preserve authorization/rejection state, and define explicit rendering/fallback policy. A plain `RE:` body fallback and a structured quote are different wire representations and both may need handling for compatibility.

Reference: [Mastodon federation docs](https://github.com/mastodon/mastodon/blob/v4.7.3/FEDERATION.md), [ActivityPub behavior](https://docs.joinmastodon.org/spec/activitypub/), [status API](https://docs.joinmastodon.org/methods/statuses/), [security/signatures](https://docs.joinmastodon.org/spec/security/).

## 7. Security requirements

Treat all remote requests, actors, activities, HTML, URLs and media as untrusted, even after signature verification.

- **SSRF/URL policy:** constrain every outbound fetch and delivery target. Reject private, loopback, link-local, unspecified, multicast and otherwise prohibited address ranges; handle DNS rebinding and validate each resolved address. Revalidate redirects and do not allow remote URLs to reach local/admin services. The crate's URL verifier is a hook, not a substitute for reviewing its exact implementation.
- **Signature and actor binding:** verify the signed request body and relevant headers; verify that the key belongs to the claimed actor; reject unknown algorithms and malformed key material. Do not infer action authorization from a valid signature.
- **Payload limits:** cap request body size, fetched response size, nesting/recursion, attachment size/count, JSON complexity and total remote fetches triggered by one request. Configure explicit timeouts.
- **HTML and URLs:** sanitize remote HTML before browser/API use; allowlist safe protocols and handle malformed markup. Never assume server-provided link, image or media URLs are safe.
- **Idempotency:** deduplicate remote ActivityPub activity IDs persistently and make each operation safe to retry. Guard against repeated Follow/Undo/Like/Delete and racing updates.
- **Authorization:** check ownership and current state for edits, deletes, follows, quote permissions, direct messages and other restricted actions.
- **Abuse controls:** protect inboxes, fetches, searches, media processing, mentions and account resolution with quotas/rate limits and bounded concurrency.
- **Secrets:** persist signing keys securely; never log private keys, access tokens or sensitive message content unnecessarily.

The crate's own documentation explicitly warns that it does not automatically sanitize received data. Build defense-in-depth at the HTTP, protocol, domain, storage and presentation layers.

## 8. Performance and redb guidance

The crate does not make timeline reads, REST serialization or redb access fast by itself. Its value is reducing federation-protocol plumbing. Design and benchmark those concerns separately.

- Keep local API reads local; do not synchronously dereference remote objects during normal timeline retrieval.
- Use explicit redb indexes for IDs, account/status relationships, time-ordered timelines, recipient delivery work and remote ActivityPub IDs.
- Keep network calls outside redb write transactions. Do not await remote HTTP while holding a write lock/transaction.
- Persist an outbox/inbox ledger with bounded job claims, retries and recovery after crashes.
- Resolve recipients once per activity where possible; deduplicate shared inboxes and avoid repeated follower queries per recipient.
- Bound concurrency and queues; apply backpressure rather than accepting unbounded work.
- Measure API latency, timeline query cost, inbox processing, remote-fetch rate, delivery throughput, retry age, and memory use independently.

Don't port a PostgreSQL-oriented data model or optional Diesel integration merely because the crate exposes one. Keep application persistence behind project-owned interfaces and implement redb-specific transaction/index behavior where appropriate.

## 9. Implementation sequence

### Phase A — dependency and compatibility spike

- [ ] Pin an exact crate version and framework version; document why that pairing was chosen.
- [ ] Review the selected release's changelog, signature code, URL verifier and inbox adapter; run the project's dependency audit.
- [ ] Build a minimal local actor and `Note`, served as ActivityPub JSON.
- [ ] Implement local WebFinger and remote handle resolution.
- [ ] Exchange a signed Follow and Create/Note with a real Mastodon v4.7.3 test instance.
- [ ] Prove required HTTP signature/key compatibility. Do not defer this until after building the whole API.
- [ ] Demonstrate duplicate inbox delivery and process restart recovery.

### Phase B — domain and durable federation plumbing

- [ ] Define project-owned domain models and protocol DTOs.
- [ ] Implement actor/object conversions and stable public IDs.
- [ ] Build persistent inbox deduplication and a transactional/recoverable outbound outbox.
- [ ] Add services for follow negotiation, status creation/update/delete, Like/Undo and Announce/Undo.
- [ ] Apply visibility and recipient-calculation tests before broadening activity coverage.

### Phase C — Mastodon REST compatibility

- [ ] Implement OAuth and the API's error/authorization semantics.
- [ ] Add account/status/relationship serializers based on v4.7.3 source and docs.
- [ ] Implement pagination, timeline visibility, notifications and endpoint-specific behavior.
- [ ] Add media, polls, lists, search, streaming and admin/moderation in separately testable modules.
- [ ] Compare responses and client flows against a reference Mastodon instance.

### Phase D — interoperability and resilience

- [ ] Test against Mastodon, Misskey/Sharkey, GoToSocial and another ActivityPub implementation.
- [ ] Test malformed/oversized activities, unknown fields, remote 404s/timeouts, DNS changes and blocked destinations.
- [ ] Test signature failures, duplicate delivery, out-of-order activities, account moves and delete/update races.
- [ ] Test delivery queue recovery after abrupt termination and storage reopen.
- [ ] Benchmark realistic feed fan-out and timeline reads; document known compatibility gaps.

## 10. Acceptance criteria

Do not describe the server as a drop-in replacement until all of the following have evidence:

- The pinned crate's current security advisories have been reviewed and regression tests cover applicable issues.
- Real Mastodon v4.7.3 instances can discover local actors, verify signatures, follow local users, deliver posts and receive edits/deletions and interaction changes.
- The server correctly applies Mastodon visibility/mention semantics and does not leak private content through shared inboxes, timelines, notifications or REST responses.
- Outbound delivery survives process restarts; inbound activity effects are idempotent.
- The supported REST endpoint list, OAuth scopes, response schemas, pagination rules, error responses and streaming events are documented and tested.
- Quote, poll, media, HTML sanitization and signature/key compatibility have explicit test coverage.
- Performance claims are backed by measurements from this implementation, not inferred from Rust or the crate choice.

## 11. Agent operating rules

When working from this document:

1. Inspect the repository's current architecture, `Cargo.toml`, `Cargo.lock`, error types and persistence interfaces before editing. This document describes desired boundaries; it does not prove the repository already has them.
2. Read the documentation/source for the exact pinned crate version. Do not copy examples from a different major/minor version without checking signatures and feature names.
3. Keep changes modular. Federation handlers call domain services; API routes call domain services; neither layer directly owns the other layer's serialization or side effects.
4. Add tests for protocol behavior and failure modes with each feature. A successful compile is not an interoperability test.
5. Do not silently stub unsupported activity types, accept every `verify` call, ignore failed delivery, or weaken SSRF/signature validation to make a test pass.
6. State unimplemented compatibility behavior explicitly. Never claim full Mastodon compatibility based on a minimal federation prototype.

## 12. Reference links

### Crate

- [Repository](https://github.com/LemmyNet/activitypub-federation-rust)
- [Stable 0.6.6 API docs](https://docs.rs/activitypub_federation/0.6.6/activitypub_federation/)
- [Stable 0.6.6 release notes](https://github.com/LemmyNet/activitypub-federation-rust/releases/tag/0.6.6)
- [Current repository manifest](https://github.com/LemmyNet/activitypub-federation-rust/blob/main/Cargo.toml)
- [CVE-2026-33693 advisory](https://github.com/advisories/GHSA-q537-8fr5-cw35)
- [In-memory delivery queue limitation](https://github.com/LemmyNet/activitypub-federation-rust/issues/31)

### Mastodon compatibility target

- [Mastodon v4.7.3 source](https://github.com/mastodon/mastodon/tree/v4.7.3)
- [Mastodon v4.7.3 federation document](https://github.com/mastodon/mastodon/blob/v4.7.3/FEDERATION.md)
- [ActivityPub specification behavior](https://docs.joinmastodon.org/spec/activitypub/)
- [Status methods](https://docs.joinmastodon.org/methods/statuses/)
- [Accounts methods](https://docs.joinmastodon.org/methods/accounts/)
- [OAuth methods](https://docs.joinmastodon.org/methods/oauth/)
- [Streaming methods](https://docs.joinmastodon.org/methods/streaming/)
- [HTTP signature/security documentation](https://docs.joinmastodon.org/spec/security/)
