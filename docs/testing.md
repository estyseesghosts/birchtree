# docs/testing.md

Status: Draft

Test matrix (per `birchtree.md` §8). Suites live in `tests/`; top-level `tests/*.rs` are compiled integration test crates. Planned subdirectories (`tests/api_contract/`, `tests/oauth/`, `tests/federation_interop/`, `tests/streaming/`, `tests/client_smoke/`) hold slices as they are implemented.

Required suites:

1. API schema snapshots — Status, Account, Relationship, Notification, Poll, MediaAttachment, ScheduledStatus, Instance, Quote; types, nullability, nested objects, decimal-string IDs.
2. Request parser — URL-encoded, multipart, JSON where allowed, nested arrays, defaults, bounds, Unicode.
3. OAuth — PKCE S256, code expiration/replay, exact redirect match, revocation, scopes, client-credentials vs user tokens.
4. Policy matrix — visibility × viewer relationship across reads, timelines, search, quotes, notifications, streaming.
5. Persistence conformance — see `docs/persistence-conformance.md`.
6. Crash tests — kill before/after commit and during workers; joint durability + no duplicate side effects.
7. Federation fixtures/interop — Mastodon v4.7.3 fixtures + one other ActivityPub implementation; signatures, audience, visibility, quote decisions, redelivery.
8. Streaming — auth, envelopes, heartbeats, backpressure, revocation, live policy changes.
9. Client tests — real Mastodon-compatible client automation where practical.
10. Performance and abuse — high-degree graphs, sparse indexes, large/malformed bodies, upload exhaustion, slow hosts, retry storms.

Gate: `bash scripts/smoke-all.sh` (`cargo check --all-targets` + `cargo test` +
`scripts/smoke-phase{1,2,3,4}.sh`) green before claiming phase completion.

## Implemented suites (Phase 1)

| Suite | File | Covers |
|---|---|---|
| Unit | `src/**` `#[cfg(test)]` | IDs/serialization/ordering, username rules, config validation, path permissions, migrations, crypto, redaction, trusted proxies |
| Persistence conformance | `tests/persistence_conformance.rs` | unique handle race, OAuth/token/session lifecycle, atomic account+job, job lease/retry/quarantine, migration + restart |
| Crash recovery | `tests/crash_recovery.rs` | `kill -9` after commit (account + job survive), aborted open (no partial state), CLI bootstrap |
| Runtime | `tests/runtime.rs` | `/health`, `/ready`, unknown-route 404 |
| Secret handling | `tests/secret_handling.rs` | no plaintext password/token/session in the DB file; token record serialization; header redaction |
| Smoke | `scripts/smoke-phase1.sh` | live binary: probes, graceful SIGTERM, CLI create-account, restart durability → `SMOKE_OK` |

## Implemented suites (Phase 2/3)

| Suite | File | Covers |
|---|---|---|
| Phase 2 API | `tests/phase2_api.rs` | instance/account DTO snapshots, registration policy, credentials/relationships reads |
| Phase 2 OAuth | `tests/phase2_oauth.rs` | PKCE S256, code replay/expiry, redirect matching, scopes, revocation, client credentials |
| Phase 3 status | `tests/phase3_status.rs` | status CRUD/source/context/history, replies/reblogs, validation strings, DTO key sets |
| Phase 3 relationships | `tests/phase3_relationships.rs` | follow lifecycle, blocks/mutes/notes/endorsements, relationship DTO, races |
| Phase 3 interactions | `tests/phase3_interactions.rs` | favourites/bookmarks/reblogs/pins/polls/quotes, counters, idempotency |
| Smoke | `scripts/smoke-phase2.sh`, `scripts/smoke-phase3.sh` | live-binary OAuth/registration and the core social loop |

## Implemented suites (Phase 4)

| Suite | File | Covers |
|---|---|---|
| Page loader + metrics | unit in `src/api/mastodon/status_view.rs` + assertions in Phase 4 suites | batched `StatusViewContext` (constant transaction count per page), store read/write/scan counters |
| Lists | `tests/phase4_lists.rs` | CRUD key sets, exact 422 bodies (blank/long title, 50-list limit, replies_policy), ownership 404s, membership follow requirement + idempotent add/remove, unfollow cascade, scopes |
| Notifications + markers | `tests/phase4_notifications.rs` | index shape (`group_key` fallback) + `Link` cursors, `types[]`/`exclude_types[]`, `limit=0`, show/dismiss/clear/`unread_count`, marker versions + marker-based counts, honest empty `requests`, scope/app-token matrix |
| Scheduled statuses | `tests/phase4_scheduled.rs` | `scheduled_at` scheduling shape, past→immediate, `Record invalid`, 5-minute floor message, CRUD + foreign 404s, durable publish (row destroyed first, replay no-op) |
| Search + emoji | `tests/phase4_search_emoji.rs` | v2 search messages (missing `q`, anonymous pagination/resolve 401s, negative limit 400), accounts/hashtags/statuses results, unknown type → empty arrays, public emoji index + image serving + CLI validation |
| Media | `tests/phase4_media.rs` (+ unit tests in `src/media/*`, `application/media.rs`) | multipart v1/v2 flows, 200/202/206/422/404 semantics, exact error bodies, processing lifecycle via the real job handler, byte-exact file serving, description/focus updates, in-usage delete |
| Filters | `tests/phase4_filters.rs` | v1/v2 CRUD key sets, exact 422 bodies, multi-keyword deprecation guard, keyword/status sub-resources, `filter_results` matching (whole_word, expiry, context narrowing) |
| Smoke | `scripts/smoke-phase4.sh` | live binary: media upload → post → timelines (Link + cache headers) → notifications + markers → lists → scheduled → search → emoji → `Vary: Authorization` → `SMOKE_PHASE4_OK` |
