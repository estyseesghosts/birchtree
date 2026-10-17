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

## Implementation status (Phase 2)

- **IDs.** `domain::id::Id` is a 64-bit snowflake (`ms << 16 | seq`), strictly
  increasing, serialized as a decimal string, deserialized from string or
  integer. `TimestampIdGenerator` is the `IdGenerator` port implementation.
- **Account/credential split.** `domain::account::Account` (ActivityPub
  identity/profile, `local`, `domain`, `actor_uri`, `moved_to`, counts) and
  `domain::user::LocalUser` (Argon2 PHC hash, email state, disabled) are
  distinct; `handle_key` builds the unique `user@domain` index key and
  `validate_username`/`normalize_username` apply Mastodon's ASCII rules.
- **OAuth records.** `domain::oauth::{OAuthApplication, AccessToken,
  AuthorizationCode}` store only the SHA-256 digest of secrets; access tokens
  default to non-expiring. `AccessToken::user_id == None` marks an app-only
  (client-credentials) token. `AuthorizationCode` binds the code to app, user,
  redirect URI, granted scopes and PKCE challenge, and is redeemable only once
  before `expires_at_ms` (`is_redeemable_at`).
- **User preferences.** `LocalUser` carries optional `default_privacy`,
  `default_sensitive`, `default_language`, `default_quote_policy`
  (`#[serde(default)]` so older records stay readable); `None` means "use the
  Mastodon default".
- **Jobs.** `domain::job::Job` is the canonical durable envelope with
  `Pending/Leased/Succeeded/Quarantined`, a due/lease timestamp, retry budget,
  and last error. Lease and backoff constants live here.
  `JobKind::SendConfirmationEmail` is committed with public registration.
- Status/relationship/quote/poll/notification types remain the Phase 0 shapes
  and are refined as their phases are implemented.

## Implementation status (Phase 3)

- **Statuses.** `domain::status::Status` carries the wire contract directly:
  `uri`/`url`, raw `source_text` + rendered `content_html`, created/edited/
  tombstone times, language, visibility, sensitive/spoiler, reply parent +
  parent author, reblog target, ordered `media_ids`, poll id, quote policy,
  resolved `mentions` + `silent_mentions` (visibility-only), lowercased
  `tags`, posting `application`, and the four counters
  (`replies/reblogs/favourites/quotes`). Edit history is
  `domain::status::StatusVersion` (version 0 = pre-edit snapshot at creation;
  later versions appended per edit; snapshots carry source/content/spoiler/
  sensitive/media/poll-titles).
- **Idempotency.** `domain::idempotency::IdempotencyRecord` (scope +
  result id, `IDEMPOTENCY_TTL_MS` = 1 hour) — Mastodon's `Idempotency-Key`
  replay for `POST /api/v1/statuses`.
- **Relationships.** `Follow` gained `FollowState {Active, Pending}` +
  `created_at_ms`; `Block`/`DomainBlock`/`Mute` (expiry + notify flag,
  `is_active_at`/`hides_notifications_at`)/`AccountNote`/`Endorsement` all
  serde-persisted. `AccountStats` holds followers/following/statuses/
  last-status/follow-request counters, mutated only inside relationship and
  status transactions.
- **Polls.** `Poll` embeds options + per-option tallies + voters/votes counts
  (single-get reads, atomic vote updates) with a sticky `expired` flag;
  `PollVote` stores the ballot's choice indices.
- **Quotes.** `QuoteRecord` implements the pinned five-state machine
  (`pending/accepted/rejected/revoked/deleted`) with both account ids;
  local quotes are accepted at creation.
- **Notifications.** `Notification` gained `created_at_ms` and the `Quote`
  kind; wire forms via `NotificationType::as_str()`. Records are created by
  the deduplicated `FanOutNotification` job after commit — never inside the
  request transaction, never duplicated by a retry.
- **Visibility/quote policy wire parsing** lives in `domain::visibility`
  (`Visibility::from_wire/as_str`, `QuotePolicy::from_wire/as_str`,
  `Default = Public`).

## Phase 4 types (media, timelines, lists, filters, markers, scheduling, emoji)

- **`MediaAttachment`** gained (all `serde(default)` so Phase 3 rows decode):
  `file_key`/`preview_key`/`thumbnail_key` (blob keys; empty while
  unprocessed), `mime_type`, `file_size`, `width`/`height`,
  `preview_width`/`preview_height`, `duration_ms`/`frame_rate`/`bitrate`
  (video/audio metadata), `focus` (`FocusPoint {x, y}`, the wire `x,y`
  param), `created_at_ms`/`updated_at_ms`, `error` (failure reason).
  `MediaState {Pending, Processing, Ready, Failed}` drives the wire contract:
  `not_processed()` ⇔ Mastodon's `processing != complete` (202/206/`url:null`).
- **`List`/`ListMembership`/`RepliesPolicy`** — title ≤256, ≤50 lists per
  owner, `replies_policy ∈ {list, followed, none}` (default `list`),
  `exclusive` flag; membership rows carry `via_request`.
- **`Marker`** — `timeline ∈ {home, notifications}`, `last_read_id`,
  optimistic `lock_version` (fresh marker reports 1), `updated_at_ms`.
  Mastodon has no per-notification read flag: this is the only read state.
- **`CustomFilter`/`FilterKeyword`/`FilterStatus`/`FilterAction`** — the
  v2 model is the single stored representation; the v1 API projects keyword
  rows. Contexts `home|notifications|public|account_list`, actions
  `warn|hide|blur`, title ≤256, keyword ≤500.
- **`ScheduledStatus`** — replayable `params` + `media_ids` +
  `scheduled_at_ms`; `TOTAL_LIMIT` 300, `DAILY_LIMIT` 25,
  `MINIMUM_OFFSET_MS` 5 min (all pinned Mastodon values).
- **`CustomEmoji`** — shortcode `[A-Za-z0-9_]{2,128}` (unique key),
  image/static blob keys, `visible_in_picker`, `disabled`, `category`;
  `listed()` mirrors `CustomEmoji.listed`.
- **Timeline query types** — `TimelineQuery` (cursors + `only_media`/
  `local_only`/`remote_only`), `TimelinePage {statuses, scanned}`,
  `TimelineFilter` (the policy-facing predicate the bounded scans apply).
