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

Not yet implemented: trait skeletons exist in `src/application/ports.rs`; redb implementations are Phase 1.
