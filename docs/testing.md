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
`scripts/smoke-phase1.sh` + `scripts/smoke-phase2.sh` +
`scripts/smoke-phase3.sh`) green before claiming phase completion.

## Implemented suites (Phase 1)

| Suite | File | Covers |
|---|---|---|
| Unit | `src/**` `#[cfg(test)]` | IDs/serialization/ordering, username rules, config validation, path permissions, migrations, crypto, redaction, trusted proxies |
| Persistence conformance | `tests/persistence_conformance.rs` | unique handle race, OAuth/token/session lifecycle, atomic account+job, job lease/retry/quarantine, migration + restart |
| Crash recovery | `tests/crash_recovery.rs` | `kill -9` after commit (account + job survive), aborted open (no partial state), CLI bootstrap |
| Runtime | `tests/runtime.rs` | `/health`, `/ready`, unknown-route 404 |
| Secret handling | `tests/secret_handling.rs` | no plaintext password/token/session in the DB file; token record serialization; header redaction |
| Smoke | `scripts/smoke-phase1.sh` | live binary: probes, graceful SIGTERM, CLI create-account, restart durability → `SMOKE_OK` |
