# docs/domain-contracts.md

Status: Draft

Domain types and invariants (per `birchtree.md` §4). These are glossary entries; exact field/nullability contracts are refined against the v4.7.3 serializers during implementation.

## Identity

- `Account`: ActivityPub identity + client-visible profile. Immutable internal identity, case-preserving + normalized username, domain/local flag, actor URI, inbox/shared inbox, profile source/fields, avatar/header media, flags, move target, local/remote state.
- `LocalUser` (credential record): password hash, email/verification state, login/session/MFA state, preferences. Distinct from `Account` and from OAuth clients. An account is not conflated with its credential.

## Statuses

- Status carries: author, source text, sanitized/rendered HTML, created/edited/deleted times, language, visibility, sensitive/CW, reply parent/account, reblog target, quote relationship, media order, poll, mentions, hashtags, application/client metadata, moderation/search flags.
- Source preserved separately from rendered `content`; source endpoints/fields exposed where the API requires.
- Replies: parent-child edges, direct-child index, cursor-bounded children reads, exact reply counters. `context` endpoint returns ancestors/descendants. Remote reply with unknown parent stays unresolved; bounded parent-resolution jobs persisted; withheld from ordinary reads until linked (unless pinned behavior demands otherwise).
- Reblogs: separate status records referencing the original; author + visibility constraints; not collapsed with quotes.
- Editing: edit history + media/poll/content updates as one atomic use case + durable notifications/distribution jobs.
- Idempotency: per-account `Idempotency-Key`, one-hour retention, key lookup, result replay, duplicate status/outbox work prevention.

## Visibility

API-visible levels: `public`, `unlisted`, `private`, `direct`. Internal `limited` is moderation/restricted state, projected as `private`, never user-creatable, never a fifth API value.

Named predicates (one domain policy feeds REST reads, timelines, search, notifications, streaming, quote previews, outgoing federation):
1. Can this viewer fetch this status?
2. Can it appear in public/local/home/list/hashtag/search results?
3. Can it be delivered to a particular ActivityPub inbox?
4. Can this viewer quote/reblog/favourite/reply to/bookmark/report it?
5. Should it be withheld (blocks, domain blocks, mutes, unavailability, filters)?

Distinct policies for show/reply/reblog/favourite/quote/pin/list-inclusion/delivery. Blocks and domain blocks affect read/interaction/distribution consistently. Mutes and filters suppress display without being a privacy boundary.

## Relationships and counters

Follow rows: `show_reblogs`, `notify`, language preferences, remote URI; locked accounts: pending follow requests + Accept/Reject federation activities.
Follows, blocks, mutes (expiry/notify), favourites, bookmarks, pins/endorsements, account notes, list membership, filters stored independently with unique keys/indexes. Local counters commit atomically with relationship changes; no eventually-incorrect local counts.

## Polls

Options, multiple-choice/hide-totals rules, expiry, votes, aggregate counts. Vote changes, repeat votes, closed polls, hidden totals, federation updates tested.

## Notifications

Durable recipient/activity relationships with types and actor/status context, read/clear/dismiss state, grouping per 4.7.3 API. Deduplicate repeated incoming activity; no duplicate notifications on job retry.

## Quotes

Internal normalized quote representation for FEP-044f, Mastodon quote activities/fields, and legacy inbound representations. External `quote`/`quote_approval` projection distinct from internal protocol provenance. States: pending/accepted/rejected/revoked/deleted. Accepted counts separate; protected targets never leaked through `quote`, preview content, or URI.

## Implementation status (Phase 1)

- **IDs.** `domain::id::Id` is a 64-bit snowflake (`ms << 16 | seq`), strictly
  increasing, serialized as a decimal string, deserialized from string or
  integer. `TimestampIdGenerator` is the `IdGenerator` port implementation.
- **Account/credential split.** `domain::account::Account` (ActivityPub
  identity/profile, `local`, `domain`, `actor_uri`, `moved_to`, counts) and
  `domain::user::LocalUser` (Argon2 PHC hash, email state, disabled) are
  distinct; `handle_key` builds the unique `user@domain` index key and
  `validate_username`/`normalize_username` apply Mastodon's ASCII rules.
- **OAuth records.** `domain::oauth::{OAuthApplication, AccessToken}` store only
  the SHA-256 digest of secrets; access tokens default to non-expiring.
- **Jobs.** `domain::job::Job` is the canonical durable envelope with
  `Pending/Leased/Succeeded/Quarantined`, a due/lease timestamp, retry budget,
  and last error. Lease and backoff constants live here.
- Status/relationship/quote/poll/notification types remain the Phase 0 shapes
  and are refined as their phases are implemented.
