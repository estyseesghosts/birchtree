# docs/persistence-conformance.md

Status: Draft (Phase 1 adapter implemented; suite green — see `docs/phase-1-exit-criteria.md`)

Persistence conformance contract for the `redb` storage adapter (per `birchtree.md` §5).

## Required record tables

Accounts, local users/credentials, statuses, follows, follow requests, blocks, domain blocks, mutes, favourites, bookmarks, pins/endorsements, account notes, lists/membership, media metadata, polls/options/votes, quote records, mentions, tags, custom emoji, notifications, OAuth applications, authorization codes, access tokens, activity/actor/key cache, inbound activity dedup, outbound deliveries, durable jobs, schema/migration metadata.

## Required indexes (first use cases)

- unique normalized local handle; handle + domain; actor URI
- status ID + canonical URI; statuses by author + descending ID; reply by parent + child ID; reblog target; quoting status ID
- public/distributable timeline range; local-status range; hashtag-to-status range
- follows both directions; follow requests both directions; blocks/mutes both directions; favourites/bookmarks by account and status; lists and membership both directions
- notifications by recipient + descending ID and activity identity
- media by owner and processing state; poll by status; votes by poll/account
- quotes by quoted status/state and by quoting status; OAuth app/token/code lookup
- jobs by due time + ID, lease and terminal/quarantine state; delivery jobs by destination/due time; dedup by canonical activity URI

Composite ordered keys for bounded prefix/range reads. No global scans, unbounded page filtering, or N+1 fetches.

## Mutation protocol

Per `architecture.md`: parse + remote lookups before the transaction; one short transaction commits primary + indexes + counters + dedup/idempotency + bounded jobs; post-commit work (media, notifications, streaming, federation) idempotent with lease settle by success/retry/defer/quarantine.

## Migrations

Each migration/index backfill: versioned, bounded, restartable, idempotent. Schema/migration metadata table tracks applied versions.

## Conformance tests (`tests/persistence_conformance.rs`)

Unique indexes, counter/index consistency, transaction atomicity, migrations,
dedup/idempotency, job lease/retry/quarantine, read-after-commit.

Phase 1 covers the tables it has implemented (accounts, handle index, local
users, OAuth apps/tokens, sessions, jobs + scheduling index) across 10 tests:
`create_and_read_back_account_and_credential`,
`duplicate_handle_is_rejected_case_insensitively`,
`unique_handle_race_has_exactly_one_winner`, `password_authentication`,
`oauth_app_and_token_lifecycle`, `login_session_lifecycle_and_expiry`,
`account_creation_commits_one_follow_up_job_atomically`,
`job_claim_complete_retry_and_quarantine`, `migrations_and_data_survive_restart`,
`crypto_helpers_reject_tampered_hash`.

## Implementation notes (Phase 1)

- The scheduling index is keyed by a fixed 16-byte `(timestamp_be || id_be)`:
  the timestamp is the due time while pending or the lease expiry while leased,
  so an abandoned lease is reclaimed with no recovery scan.
- Concurrent writers are serialized by `redb`'s single write slot; the handle
  uniqueness check runs inside the write transaction, so races resolve to one
  winner (proven by `unique_handle_race_has_exactly_one_winner`).
- Values are JSON bytes; keys are ordered primitives. JSON is used so records
  can gain fields without a key-layout migration.
- `kill -9` durability is proven by `tests/crash_recovery.rs` against the real
  binary (`crash-harness` mode): a committed account + its job survive; an
  aborted open leaves no partial state.
