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

Each migration/index backfill: versioned, bounded, restartable, idempotent. Schema/migration metadata table tracks applied versions. Current `SCHEMA_VERSION` is 16 (`src/persistence/redb/schema.rs:560`); the quote-cursor rekey landed in v16 and the confirmation/approval identity stores in v15 (see `docs/endpoint-manifest.md` and `docs/compatibility-profile.md` D36).

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

## Implementation notes (Phase 3 — schema v3)

The v3 migration adds (one write transaction per migration step, restartable):

- **Records:** `statuses`, `status_versions`, `status_pins`, `polls`,
  `poll_votes`, `quotes`, `notifications`, `favourites`, `bookmarks`,
  `follows`, `blocks`, `domain_blocks`, `mutes`, `account_notes`,
  `endorsements`, `idempotency`, `account_stats`, `media`.
- **Indexes:** `status_author_index` (`author_be || desc(id)` for
  descending scans), `status_parent_index`, `status_reblog_index`,
  `reblogs_by_account`, `poll_by_status`, `quotes_by_quoted` /
  `quotes_by_quoting`, `favourites_by_status` / `bookmarks_by_status`
  (`status || interaction_id` for cursor order), `follows_by_followed`,
  `blocks_by_blocked`, `mutes_by_muted`, `endorsements_by_endorsed`,
  `mentions_by_account`, `hashtag_index` (`tag || 0xFF || status`),
  `notifications_by_recipient` (`recipient || desc(id)`),
  `notification_dedup` (40-byte interaction-instance key), `media_by_owner`.
- Composite keys are fixed-width big-endian pairs built by
  `schema::pair_key`/`desc_id`; `domain_block_key` separates the account
  prefix from the domain with `0xFF` (never present in a domain). Ordering is
  numeric everywhere — decimal IDs are never compared lexicographically.
- Counters live on the record they describe (`Status.*_count`,
  `Poll.votes/voters_count`) or in `account_stats`, and are written inside the
  same transaction as the row that changes them, so reads cannot observe a
  counter/row disagreement.
- Idempotency records (`idempotency`, 24-byte `account || sha256` key,
  1-hour TTL enforced at read) commit in the same transaction as the status
  they protect.

Conformance tests added with Phase 3 (in `tests/persistence_conformance.rs`):
status create commits record + indexes + jobs atomically (and an aborted
create leaves neither), notification-fanout job idempotence (dedup), natural
interaction idempotence (repeat favourite → one row/one job), `Idempotency-Key`
replay within the hour, and reopen-after-crash durability of an interaction +
job with post-restart job completion producing exactly one notification.

## Implementation notes (Phase 4 — schema v4)

The v4 migration adds these tables and, crucially, **backfills** the timeline
ranges from existing statuses (public + not deleted → `timeline_public`;
author account `local` → `timeline_local`; bounded by the pre-v4 status count
and idempotent):

- **Records:** `lists`, `list_members`, `markers`, `filters`,
  `filter_keywords`, `filter_statuses`, `scheduled_statuses`,
  `custom_emoji`.
- **Indexes:** `lists_by_account` (`account || desc(list)`),
  `memberships_by_account` (reverse, for unfollow/block cleanup),
  `filters_by_account`, `filter_keywords_by_filter` /
  `filter_statuses_by_filter`, `scheduled_by_account` (`account || desc(id)`),
  `timeline_public` / `timeline_local` (`desc(id) → ()`).
- **Timeline index maintenance:** `write_status_txn` (the single row writer
  used by create, reblog, edit, counter bumps and tombstones) keeps
  `timeline_public`/`timeline_local` in step: public non-deleted statuses
  join both ranges when the author account is local (localness read from
  `accounts` inside the same transaction); tombstones and non-public
  visibilities remove the entries. Every step is idempotent.
- **Follow cascade:** `unfollow`, `remove_follower` and `block` drop the
  affected `list_members` rows (`lists::retain_membership_txn`) in the same
  transaction as the follow removal — mirroring upstream's
  `list_accounts.follow_id ON DELETE CASCADE`.
- **Media lifecycle:** `create_media_upload` commits metadata + owner index +
  the `ProcessMedia` job in ONE transaction (bytes are staged in the
  `BlobStore` *before* commit); `mark_media_processed` commits the
  processing result idempotently; `delete_media` drops row + owner index
  (bytes are removed by the use case after commit).
- **Scheduled statuses:** `create_scheduled` commits row + account index +
  the `PublishScheduledStatus` job (due at `scheduled_at`) in ONE
  transaction; publishing destroys the row *before* replaying params, so a
  crash mid-publish never republishes (replay after deletion is a no-op).
- **Markers:** optimistic `lock_version` checked inside the write
  transaction; a stale version rejects with `MarkerUpdateError::Stale`
  (409 on the wire).

### Documented scan caps (bounded reads)

| Path | Cap |
|---|---|
| public/tag timeline index walk | 2 000 entries |
| hashtag timeline index walk | 2 000 entries |
| home/list k-way merge | lazy merge: one cursor per author and a heap of cursor heads; 5 000 index entries per call, cursor heads counted; 1001 authors (viewer + 1000 home follows, Phase 4.1) |
| notification page / clear / count | 1 000 entries (clear 10 000) |
| account lists scan | 100 |
| list members scan | 2 000 (timeline member list 500) |
| filters per account / rules per filter | 500 / 1 000 |
| scheduled rows per account / table sweep | 320 / 1 000 |
| account search (handle index) | 5 000 |
| tag search (hashtag index) | 5 000 entries / 1 000 distinct |
| recent-status search | `cap` entries (4× rows) |
| emoji index | 1 000 |

### Instrumentation

`RedbStore` counts `read_txns`, `write_txns` and `items_scanned` (exposed via
`RedbStore::metrics()`); every counted `begin_read`/`begin_write` and every
bounded range scan increments them. The Phase 4 suites assert page-level
bounds (e.g. one home page ≤ 5 000 scanned entries) and use the deltas as
executed evidence for the no-unbounded-scan rule.
