# Phase 3 — exit criteria checklist

Source: `birchtree.md` §7, Phase 3. Contracts: `docs/endpoint-manifest.md`,
`docs/domain-contracts.md`, `docs/visibility-and-authorization.md`,
`docs/application-ports.md`, `docs/persistence-conformance.md`.

**Exit:** tests cover local flows, privacy boundaries, relationship races,
exact DTOs, counters and idempotency; crash after commit still completes
follow-up work; a client can perform the core social loop **without
federation**.

## Statuses

- [x] Create/show/delete, `source`, `context` (ancestors/descendants),
  multi-fetch (`id[]` ≤ 20, silent omission, id-desc), editing + `history`
  (version 0 snapshot on first edit; no-op edit does not bump `edited_at`),
  replies (`replies_count`, parent resolution with the exact 404 message),
  reblogs/unreblog (wrapper record, idempotent re-reblog). Evidence:
  `tests/phase3_status.rs`, `scripts/smoke-phase3.sh`.
- [x] Status DTO snapshot matches the pinned `REST::StatusSerializer` key set
  and nullability (incl. per-viewer flags only with a token, `pinned` only
  when pinnable, `text` instead of `content` on DELETE).
- [x] Validation strings match pinned en.yml (blank text, 500-char limit with
  URL=23, poll option/expiration rules, visibility/language).

## Visibility, relationships, interactions

- [x] Privacy matrix (public/unlisted/private/direct × anonymous/author/
  follower/mentioned/unrelated/blocked) exercised over the real endpoints.
- [x] Follow lifecycle incl. locked-account follow requests
  (authorize/reject), self-follow 403, blocked-pair 403, block performs mutual
  unfollow + endorsement cleanup, mute `duration`/`notifications`, account
  notes (≤2000, blank deletes), endorsements (follow required), domain blocks.
- [x] Relationship race test: concurrent follow/unfollow settles to exactly
  one row and correct counters.
- [x] `REST::RelationshipSerializer` exact key set incl. neutral defaults
  (`showing_reblogs:false` without a row) and real counters on account DTOs.
- [x] Favourites/bookmarks/pins: idempotent retries, counters never negative,
  four exact pin 422 messages, pin limit 5, `pinned` flag + `?pinned=true`.
- [x] Notification records are committed via deduplicated jobs — retries never
  duplicate a notification; a fresh interaction notifies again.

## Polls, media relations, application metadata

- [x] Poll create/vote/expiry flags: `choices[]` required (400), the four vote
  422 messages, self-vote, single-choice re-vote rejection, multiple-poll
  add-a-choice semantics, `hide_totals` masks only per-option counts.
- [x] Media attachment relations/order validated (ownership, ≤4, ordered
  `media_ids`, DTO `media_attachments` order, `url:null` while unprocessed).
- [x] Sensitive/CW/language, application metadata, status counts and
  per-viewer relationship flags present in the DTOs.

## Quotes

- [x] Quote state machine: local auto-accept, `quote` DTO with
  `quoted_status` (accepted only) + shallow nesting one level down,
  `quotes_count` transitions, revoke by quoted author only, quote list.
- [x] `quote_approval_policy` defaults (user preference), create-time
  validation (`public|followers|nobody`, 422 on garbage), forced `nobody`
  for private/direct, interaction-policy endpoint round-trip,
  `quote_approval {automatic, manual, current_user}` semantics.
- [x] Local quote authorization enforced before any federated quote handling
  (denied quotes answer the pinned 404; direct-quote mention rule).

## Idempotency and durability

- [x] `Idempotency-Key` replay returns the original status (200) within 1h,
  per-account namespacing; replay does not double counters or jobs.
- [x] Repeated interaction retries do not duplicate status/interaction/
  notification/job rows (store-level tests).
- [x] Crash after commit still completes follow-up work: interaction + job
  survive store reopen; running the handler twice yields exactly one
  notification.

## Cross-cutting

- [x] Schema v3 migration (v2 → v3 upgrade test; all tables listed in
  `docs/persistence-conformance.md`).
- [x] Pagination widget wired into the new paginated endpoints with `Link`
  headers (statuses-by-account, followers/following, quotes,
  favourited/reblogged-by, follow requests, endorsements, domain blocks).
- [x] Endpoint manifest matches reality: implemented rows tagged
  `implemented (Phase 3)`, absent rows `planned (Phase N)`, exclusions
  reworded from `stubbed`; known deviations listed.
- [x] `scripts/smoke-phase3.sh` passes the core social loop against the live
  binary; `bash scripts/smoke-all.sh` ends `smoke-all: green`.

## Evidence (executed)

Executed on the release-relevant baseline (2026-10-17), full output pasted in
`docs/logs/2026-10-17-phase-3-verification.md`:

- `cargo check --all-targets` — green, zero warnings.
- `cargo test` — **138 passed, 0 failed**: 51 unit; 3 `crash_recovery`;
  18 `persistence_conformance` (+5 Phase 3: atomic create+jobs, notification
  dedup, interaction replay, idempotency replay, reopen-and-complete);
  13 `phase2_api`; 13 `phase2_oauth`; 8 `phase3_interactions`;
  13 `phase3_relationships`; 13 `phase3_status`; 3 `runtime`;
  3 `secret_handling`.
- `scripts/smoke-phase3.sh` — live binary via curl: full social loop
  (create/show/edit/history/source/context/delete, idempotent retries,
  multi-fetch, favourite/bookmark/reblog with counters, follow + locked
  follow-request lifecycle, block/mute/note/endorse, visibility matrix,
  pins, poll + votes + hide_totals, quotes + revoke + interaction policy,
  domain blocks) → `SMOKE_PHASE3_OK`.
- `bash scripts/smoke-all.sh` — `smoke-all: green` (check + test + phase1 +
  phase2 + phase3).

Product bugs found by the Phase 3 suites and fixed (recorded for audit):
bodyless-POST `415` (lenient form extractor), `can_fetch` ignoring author
blocks on public/unlisted, neutral `showing_reblogs` default, single-choice
re-vote accepted (Mastodon rejects any second vote), follow-requests index
scope missing `follow`, multi-fetch not deduplicating `id[]`,
`status_source_json` leaking `null` spoiler, fabricated poll expiry strings
(real: `Deadline is too soon` / `Deadline can't be blank` / `Choices ...`),
pin ownership answering 403 instead of 422 `Validation failed: ...`,
viewer flags not resolving through `status.proper`, unreblog leaving the
account→reblog mapping behind.

## Open / deferred (explicit)

- Media **bytes**/serving, timelines, notifications read surface → Phase 4;
  streaming → Phase 5; federation (incl. remote quote requests) → Phase 6.
- JSON request bodies on REST writes are not accepted (form/empty only) —
  recorded in the manifest's honesty list.
- Tag objects use the tag name as `id`; no tag table until Phase 7.
- Conversation mutes (`statuses/:id/mute`) out of profile with conversations;
  DTO `muted` flag is a neutral `false`.
- Phase 2 carry-overs unchanged: email confirmation not enforced;
  invite/approval modes; registration `locale`/`time_zone`/`date_of_birth`
  ignored; profile `fields_attributes`/`discoverable`/`indexable`/
  `hide_collections` still not persisted; `accounts/lookup` + `/search`
  deferred to Phase 7 (manifest says `planned (Phase 7)`).
- Notifications are *created* (deduplicated, job-driven) but have no read
  surface yet — Phase 4.

## Status 2026-10-20 (freshness pass, worker W1)

Three "Open / deferred" bullets above are stale at HEAD and are superseded,
not rewritten: JSON request bodies on REST writes ARE accepted
(`src/api/json_body.rs:30-33`, `LenientForm` in `src/api/forms.rs:131-156`,
shipped in Phase 4.1); profile `fields_attributes`/`discoverable`/
`indexable`/`hide_collections` ARE persisted (see the 2026-10-20 note in
`docs/phase-2-exit-criteria.md`); `accounts/lookup` + `/search` ship in
Phase 7, not "deferred to Phase 7" as future work
(`src/api/mastodon/router.rs:66-67`). Conversation mutes + notification read
surface shipped in Phase 4/7 (`src/api/mastodon/router.rs:270-275`). Tag
objects still use the tag name as `id` (no tag table) unless a later phase
says otherwise. Of the Phase 2 carry-overs: invite/approval modes are now
implemented (see the 2026-10-20 note in `docs/phase-2-exit-criteria.md`);
email-confirmation enforcement, min-age policy and `locale`/`time_zone`/
`date_of_birth` handling remain as stated above.
