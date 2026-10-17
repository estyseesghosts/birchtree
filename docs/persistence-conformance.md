# docs/persistence-conformance.md

Status: Draft

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

## Conformance tests (planned, `tests/persistence_conformance.rs`)

Unique indexes, follow/reaction uniqueness, counter consistency, range pagination, transaction atomicity, migrations/backfills, dedup/idempotency, job lease/retry/quarantine, read-after-commit. No redb run yet (Phase 1).
