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
declared in `application::ports` and implemented by
`mail::LoggingMailSender` (no SMTP transport yet). `StatusStore`,
`RelationshipStore`, `PollStore`, `MediaStore`, `NotificationStore`,
`ActivityDedup`, `BlobStore`, `StreamingSink`, and `FederationHttp` remain to be
implemented in their phases.

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
  used by status create/edit (bytes remain Phase 4).

`ActivityDedup`, `BlobStore`, `StreamingSink`, `FederationHttp` remain to be
implemented in their phases; `NotificationStore`'s read surface (index,
grouping) is Phase 4.
