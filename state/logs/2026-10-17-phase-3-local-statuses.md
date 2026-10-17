# 2026-10-17 — Phase 3: local statuses, relationships and interaction semantics

## What changed

Vertical slices end-to-end through domain → application → persistence → REST,
each with tests; no federation (Phase 6), no timelines/media bytes/notifications
read surface (Phase 4).

**Domain (`src/domain/`)**

- `Status` reshaped to the wire contract: `uri`/`url`, source + rendered HTML,
  created/edited/tombstone times, reply parent + author, reblog target,
  ordered `media_ids`, quote policy, resolved `mentions` + `silent_mentions`,
  `tags`, posting `application`, and the four counters. New `StatusVersion`
  (edit history), `StatusApplication`.
- `Follow` gained `FollowState {Active, Pending}`; `Block`/`DomainBlock`/
  `Mute`/`AccountNote`/`Endorsement` moved into `domain::relationship.rs`
  with serde (`domain::block.rs`/`domain::mute.rs` removed). `AccountStats`
  holds per-account counters.
- `Poll` embeds options + tallies (`hide_totals` masking stays serializer-
  side); `PollVote` stores choice indices. `QuoteRecord` implements the
  pinned five-state machine. `Notification` gained `created_at_ms` + the
  `Quote` kind. New `IdempotencyRecord` (1-hour TTL, per-account).
- `Visibility`/`QuotePolicy` wire parsers (`from_wire`/`as_str`),
  `Default = Public`.
- `JobKind::FanOutNotification` + `JobKind::PollExpired` with idempotent
  handlers (`jobs/handlers.rs` now takes the store).

**Application (`src/application/`)**

- `ports.rs`: new `StatusStore`, `RelationshipStore`, `PollStore`,
  `QuoteStore`, `NotificationStore`, `MediaStore` bundled into `Store`;
  frozen input/output structs (`NewStatus` incl. idempotency key,
  `StatusUpdate`, `AccountStatusesQuery`, `FollowOptions`/`FollowOutcome`,
  `Relationship`, `PairState`, `VoteRejection`, `NewNotification`).
- `policy.rs` (new): the single visibility/interaction policy
  (`can_fetch`/`can_reblog`/`can_favourite`/`quote_permission`) with unit tests.
- `statuses.rs`: create (validation order matching `PostStatusService`:
  CW folding, media ≤4, visibility/language defaults, URL=23 length counting,
  reply/quote resolution incl. reblog unwrap + direct-mention rule +
  policy-driven visibility downgrade, mention/tag extraction + local-only
  resolution, poll validation), show/source/context/history/edit/delete,
  multi-fetch, account statuses; rendering helpers (`render_html`).
- `relationships.rs`: follow/request state machine, unfollow,
  remove-from-followers, block/unblock, mutes with duration, notes,
  domain blocks, endorsements, list reads, relationship aggregates.
- `interactions.rs` (new): favourite/bookmark/reblog/unreblog/pin/unpin with
  policy gates. `polls.rs` (`PollVoteError`), `quotes.rs` (list/revoke/
  interaction policy). `notifications.rs`: job-payload fan-out.

**Persistence (`src/persistence/redb/`)** — schema **v3** + migration step +
`upgrades_v2_database_to_v3` test. New tables: `statuses` + author/parent/
reblog/reblogs-by-account indexes, `status_versions`, `status_pins`,
`favourites`/`bookmarks` + reverse indexes, `follows` (+reverse), `blocks`
(+reverse), `domain_blocks`, `mutes` (+reverse), `account_notes`,
`endorsements` (+reverse), `polls`, `poll_by_status`, `poll_votes`, `quotes`
(+2 indexes), `notifications` (+recipient index + 40-byte interaction-instance
`notification_dedup`), `idempotency`, `account_stats`, `media` (+owner index),
`mentions_by_account`, `hashtag_index`. Every mutating method commits record +
indexes + counters + jobs in ONE write transaction; helper
`jobs::insert_job(&write, …)` reused; bounded scan caps documented.

**REST (`src/api/mastodon/`)**

- `endpoints/statuses.rs` (new): index (multi-fetch), create (incl.
  `Idempotency-Key` header), show, update (PUT/PATCH), delete (source-mode
  body), source, context, history, favourite/unfavourite, bookmark/
  unbookmark, reblog/unreblog (with `visibility`), pin/unpin,
  favourited_by/reblogged_by (interaction-id cursors in `Link`),
  quotes index + revoke, interaction policy, account statuses.
- `endpoints/relationships.rs` (new): follow/unfollow/remove-from-followers,
  block/unblock, mute/unmute, note, endorse/pin + unendorse/unpin (4 aliases),
  follow_requests index/authorize/reject, domain blocks GET/PUT/DELETE,
  followers/following/endorsements lists — all paginated with the
  Phase-2-built pagination widget (`Link` headers; domain-string cursors for
  domain blocks).
- `endpoints/polls.rs` (new): show + votes.
- `status_view.rs` (new): viewer-aware `StatusView` assembly (author + stats,
  proper-resolved flags, media/mentions/poll+ballot, quote-state resolution
  incl. `unauthorized`/`muted_account`/`deleted`).
- `dto.rs`: `status_json` (full pinned key set, per-viewer fields only with a
  token, `pinned` only when pinnable, shallow quote nesting one level down,
  `quote_approval`), `poll_json(_with_ballot)` (hide_totals masks only
  per-option counts), `media_attachment_json` (`url:null` while unprocessed),
  `status_source_json`, `status_edit_json`, stats-aware
  `account_json_with_stats`/`credential_account_json_with_stats`, real
  `relationship_json`.
- `api/forms.rs`: `LenientForm` extractor (bodyless POSTs are form-empty, not
  415).
- Router registers all Phase 3 routes; unimplemented routes remain absent
  (404).

**Scripts/docs**

- `scripts/smoke-phase3.sh` (new): live-binary social loop →
  `SMOKE_PHASE3_OK`; wired into `scripts/smoke-all.sh`.
- Contracts updated: `endpoint-manifest.md` (all Phase 3 rows
  `implemented (Phase 3)`; `stubbed` wording replaced by `planned (Phase N)`;
  new honesty list of deviations), `visibility-and-authorization.md`,
  `application-ports.md`, `persistence-conformance.md`, `domain-contracts.md`,
  `compatibility-profile.md`, `testing.md`, `README.md`, wiki
  (`Roadmap`/`Feature-Matrix`/`Home`), new `docs/phase-3-exit-criteria.md`.

## What was verified (executed)

- `cargo check --all-targets` — green, zero warnings.
- `cargo test` — **138 passed, 0 failed** (51 unit; crash 3; conformance 18;
  phase2_api 13; phase2_oauth 13; phase3_interactions 8;
  phase3_relationships 13; phase3_status 13; runtime 3; secret_handling 3).
- `scripts/smoke-phase3.sh` → `SMOKE_PHASE3_OK`; `bash scripts/smoke-all.sh`
  → `smoke-all: green`.
- Full pasted output: `docs/logs/2026-10-17-phase-3-verification.md`.

## Bugs found by the suites during this session (all fixed)

Recorded with details in `docs/logs/2026-10-17-phase-3-verification.md`:
bodyless-POST 415, `can_fetch` ignoring author blocks, neutral
`showing_reblogs` default, single-choice re-vote accepted, follow-requests
scope list, multi-fetch id dedup, `spoiler_text:null` in source responses,
fabricated poll error strings (canonical: `Deadline ...` / `Choices ...`),
pin ownership 403→422, viewer flags not resolving through `status.proper`,
unreblog leaving the account→reblog mapping behind.

## Open questions / deferrals

- Notification *records* are created (deduplicated, job-driven) but have no
  read surface until Phase 4; mutes suppress only DTO/flags for now (feed
  suppression is a Phase 4 read-surface concern).
- Poll expiry is enforced lazily (read-time) + by the `PollExpired` sticky
  job; no notification on poll end (Mastodon's `PollExpirationNotifyWorker`
  maps to Phase 4).
- JSON request bodies on REST writes remain unsupported (form/empty only) —
  manifest honesty list; Phase 7 request-parser sweep.
- Tag objects use the tag name as `id` (no tag table until Phase 7);
  conversation mutes out of profile with `conversations`.
- `GET blocks/mutes/favourites/bookmarks` collection reads stay Phase 7.
- Phase 2 carry-overs unchanged (email confirmation, invite/approval modes,
  profile fields, lookup/search now tagged `planned (Phase 7)`).

## Next step

Phase 4: media (bytes/upload/serving), timelines with cursor pagination and
viewer-aware filtering, notification read surface (records already exist),
`StatusViewContext` page-loader measurements.
