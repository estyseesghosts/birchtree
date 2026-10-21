# docs/application-ports.md

Status: Draft

Application ports (per `birchtree.md` §3/§4). Domain code depends only on these trait definitions; adapters implement them.

| Port | Responsibility | Owner |
|---|---|---|
| `Clock` | monotonic/wall time + ID timestamp source | `domain::id` |
| `IdGenerator` | 64-bit timestamp-oriented Mastodon-shaped IDs | `domain::id` |
| `AccountStore` | accounts, local users, credentials | `persistence::redb` |
| `StatusStore` | statuses, replies, reblogs, quotes, indexes | `persistence::redb` |
| `RelationshipStore` | follows, blocks, mutes, favourites, bookmarks, lists | `persistence::redb` |
| `PollStore` | polls, options, votes | `persistence::redb` |
| `MediaStore` | media metadata, processing lifecycle | `persistence::redb` + `media::blob_store` |
| `NotificationStore` | notifications, read/clear/dismiss | `persistence::redb` |
| `OAuthStore` | apps, authorization codes, access tokens | `persistence::redb` |
| `JobQueue` | durable jobs: due-time + ID, lease, terminal/quarantine | `persistence::redb` + `jobs::queue` |
| `ActivityDedup` | inbound activity ID dedup | `persistence::redb` |
| `BlobStore` | media bytes on local object storage | `media::blob_store` (filesystem) |
| `StreamingSink` | publish stream events to connected clients | `streaming::hub` |
| `FederationHttp` | safe outbound HTTP (SSRF, timeouts, redirects, size) — provided via `activitypub-federation-rust` where it covers the concern | `federation::delivery` |
| `MailSender` | verification/password mail (adapter/job, not in request txn) | adapter |

Mutation rule: one storage transaction commits primary records, indexes, local counters, dedup/idempotency records, and bounded durable jobs atomically. Media processing, notifications, streaming publication, outbound federation run after commit (see `architecture.md`).

## Implementation status (Phase 2)

`Clock` and `IdGenerator` are implemented in `domain::id`. `AccountStore`,
`OAuthStore`, `SessionStore`, `JobQueue`, and `ReadyCheck` are implemented by
`persistence::redb::RedbStore`, which also satisfies the bundled `Store`
supertrait assembled into `application::context::Context`. `MailSender` is
declared in `application::ports` and delivered for real via `lettre` when
SMTP is configured (round 4; unconfigured = explicit 503, never fake
success). Every store port named below shipped in its phase
(`StatusStore`/`RelationshipStore`/`PollStore`/`MediaStore`/
`NotificationStore` incl. the Phase 4 read surface, `ActivityDedup`,
`BlobStore`, `StreamingSink`, `FederationHttp`).

Phase 2 additions to the narrow ports:

- `AccountStore::update_account_profile`, `AccountStore::update_user_settings`.
- `OAuthStore::app_by_id`, `OAuthStore::create_authorization_code`,
  `OAuthStore::authorization_code_by_plaintext`,
  `OAuthStore::redeem_authorization_code` (one transaction marks the code used
  and writes the token).
- `NewLocalAccount::send_confirmation_email` commits a
  `SendConfirmationEmail` job in the account transaction.
- `UserSettingsUpdate` / `AccountProfileUpdate` carry `PATCH
  update_credentials` patches; `None` fields are left unchanged.
  `AccountProfileUpdate` also carries the avatar/header slots (`None` =
  unchanged, `Some(None)` = clear to the default, `Some(Some(..))` =
  reference the staged blob) plus the image descriptions; the redb commit
  enqueues one `FederationEvent {event: "profile_update"}` in the same
  transaction (no-op for remote rows).

## Implementation status (Phase 3)

Implemented by `persistence::redb::RedbStore` and bundled into `Store`:

- `StatusStore` — create (record + poll/quote + indexes + counters +
  idempotency record + follow-up jobs in ONE transaction), read, edit
  (versioned snapshots), tombstone, thread children, per-account/pinned
  listings, batch viewer flags, and the interactions (favourite, bookmark,
  reblog, pin, quote-policy update), each ONE transaction with counters and a
  `FanOutNotification` job where applicable. `NewStatus.idempotency_key`
  implements Mastodon's `Idempotency-Key` (1-hour replay).
- `RelationshipStore` — follow/follow-request state machine, unfollow,
  remove-follower, blocks (mutual unfollow + endorsement cleanup in one
  transaction), domain blocks, mutes with duration, account notes,
  endorsements (follow required), follower/following lists, the
  `Relationship` aggregate, `pair_state` for the policy module, and
  `account_stats`.
- `PollStore` — poll reads, `vote_poll` (validation inside the transaction:
  expiry, self-vote, choices, single-choice re-vote), ballot lookup,
  `mark_poll_expired` (job handler, idempotent).
- `QuoteStore` — quote lookup, accepted-quote listing, revoke state machine
  (accepted → revoked with quoted-counter decrement, else rejected) in one
  transaction.
- `NotificationStore::create_notification` — insert-with-dedup: the 40-byte
  key `(recipient, type, actor, status, created_at)` deduplicates replays of
  one interaction while allowing a fresh interaction to notify again.
- `MediaStore` — attachment metadata rows + ownership/attachment validation
  used by status create/edit (bytes staged since Phase 4; transcoding
  pipeline since Phase 7.1).

All narrow ports shipped in their phases (see the phase exit criteria).

## Implementation status (Phase 5)

- `StreamingSink` (`application::ports`) is implemented by `streaming::Hub`. It is one method, `publish(StreamEvent)`, non-blocking: events go to the hub's fan-out thread through a bounded queue, and an overflow closes every stream. The event type is `application::streaming::StreamEvent` (`StatusCreated`, `StatusEdited`, `StatusDeleted` with pre-deletion snapshots, `Notification`, `PolicyChanged`, `Retract`, `TokenRevoked`). `Context` carries the sink as `streaming`. Adapters publish after a use case returns `Ok`; the job handler publishes after its own commit. No use case publishes inside a transaction.
- `StatusStore::delete_status` returns `DeletedStatus { status, reblogs }`: the pre-deletion status and every reblog wrapper the cascade tombstoned (for `delete` events). Previously it returned the status alone.
- `StatusStore::own_reblog(account_id, status_id)`: the viewer's live reblog wrapper of a status (for the unreblog and repeat-reblog decisions).
- `NoStreaming` is a sink that discards events, for code paths with no hub.
- Stream gates (read-only, in `application::timelines`): `home_includes`, `public_stream_includes`, `tag_stream_includes`, `list_stream_includes`, and `retraction_candidates` (run before a block or mute commits). They reuse `ViewerScope` and the timeline filters; streaming adds no policy of its own.
- `jobs::handlers::handle` and `jobs::worker::run` take the sink. `fan_out_from_payload` and `publish_scheduled_job` return the id they created, so the handler publishes it once.

## Implementation status (Phase 4)

New ports in `application::ports`, implemented by `persistence::redb::RedbStore`
and bundled into `Store`:

- `BlobStore` (filesystem `media::blob_store::FsBlobStore`, traversal-checked
  keys) — staged bytes before every media/emoji metadata commit.
- `MediaStore` additions — `create_media_upload` (row + owner index +
  `ProcessMedia` job in ONE transaction), `owner_media`, `update_media`,
  `delete_media`, `mark_media_processed`, and the batched `media_by_ids`.
- `ListStore` — list CRUD (ownership 404s inside the store), membership add/
  remove with the owner-follow validation, member listings (cursor + full),
  and the unfollow/block cascade helper used by `RelationshipStore`.
- `MarkerStore` — marker reads and the optimistic-locked upsert
  (`MarkerUpdateError::{Stale, InvalidTimeline}`).
- `FilterStore` — v2 filter CRUD with keyword/status rule sub-resources; the
  v1 API projects keyword rows (one stored representation).
- `ScheduledStore` — schedule (row + index + `PublishScheduledStatus` job),
  cursor listing, reschedule, delete, due-sweep and the `(total, today)`
  counts behind Mastodon's 300/25 limits.
- `EmojiStore` — shortcode-keyed upsert/delete, listed index (bounded) and
  shortcode batch lookup for payload embeddings.
- `TimelineStore` — `public_timeline`, `hashtag_timeline`, `authors_timeline`
  (fan-out-on-read k-way merge): each returns `TimelinePage { statuses,
  scanned }` after applying the caller's `TimelineFilter` *inside* its bounded
  scan (policy stays in `application::timelines`).
- `SearchStore` — bounded `search_accounts` / `search_tags` / `recent_statuses`
  scans behind `GET /api/v2/search`.
- `JobQueue::enqueue_at` — due-time scheduling for `PublishScheduledStatus`.
- `NotificationStore` read surface — `notifications_page` (cursors, bounded),
  `notification_by_id`, `delete_notification`, `clear_notifications`,
  `count_notifications_after` (marker-based `unread_count`).
- Page-loader batches (used by `StatusViewContext`): `AccountStore::
  accounts_by_ids`, `RelationshipStore::{account_stats_by_ids,
  batch_pair_states}`, `PollStore::{polls_by_ids, ballots}`,
  `QuoteStore::quotes_by_quoting_ids`.
