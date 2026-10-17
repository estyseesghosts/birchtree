# Current state

Status: 2026-10-17, Phase 4.1 (parity gap closure) implemented; Phase 5 (streaming) is next
implemented (gate green); Phase 5 (streaming) next.

## Implemented

Repository and contracts:

- Repository layout per `birchtree.md` §3 (one Cargo package, `domain` ← `application` ← adapters)
- `AGENTS.md`, `docs/` contracts, `docs/wiki/`, append-only `state/logs/` and `docs/logs/`
- `scripts/smoke-all.sh` full gate (check + test + phase1/2/3/4 smokes);
  `.github/workflows/ci.yml` release-build CI

Runtime (Phase 1):

- Structured `tracing` logging with `RUST_LOG` filtering and secret redaction
- `GET /health` and `GET /ready` probes; unknown routes return 404
- Graceful shutdown on SIGINT/SIGTERM; configuration validation; data-dir
  path permissions (`0700`); body limits; trusted-proxy client-IP handling
- CLI: `serve`, `create-account` (plus test-only `crash-harness`)
- Mastodon-shaped snowflake IDs (numeric ordering, decimal-string wire) and a
  monotonic `Clock`
- Versioned `redb` schema + migration runner (now at **v5**: domain-block row ids
  and id index (2026-10-17); v4 added Phase 4 timeline
  index + backfill), narrow
  persistence ports, atomic mutation + job commits, durable job queue with
  lease/backoff/quarantine, in-process worker

OAuth, registration and read APIs (Phase 2):

- `POST /api/v1/apps`, `GET/POST /oauth/authorize` (login + consent),
  `POST /oauth/token` (PKCE S256 + client-credentials), `POST /oauth/revoke`
- `POST /api/v1/accounts` registration (open/closed policy), account
  show/verify/update credentials, `GET /api/v1/instance`, `/api/v2/instance`,
  `/api/v1/preferences`
- Scope enforcement (any-of), Mastodon error shapes, rate-limit headers,
  form parsing, pagination widget

Local statuses, relationships, interactions (Phase 3):

- **Statuses:** create/show/edit/delete, `source`, `context`, `history`,
  multi-fetch (`id[]`), replies, reblogs/unreblog, pins, `favourited_by`/
  `reblogged_by`, exact `REST::StatusSerializer` DTO (per-viewer flags,
  `pinned` only when pinnable, source-mode delete body), `Idempotency-Key`
  replay (1 h, per account)
- **Visibility/policy:** one policy module (`application/policy.rs`) for
  fetch/reblog/favourite/bookmark/quote gates across public/unlisted/private/
  direct with blocks, follows and mentions; privacy-matrix tested
- **Relationships:** follow (locked → follow requests with list/authorize/
  reject), unfollow, remove-from-followers, blocks (mutual unfollow +
  endorsement cleanup), domain blocks (upstream shape since 2026-10-17:
  newest first, row-id paging), mutes with duration/notifications,
  account notes, endorsements, follower/following lists, real
  `REST::RelationshipSerializer` aggregates, per-account counters
  (`account_stats`) updated atomically
- **Interactions:** favourites/bookmarks (idempotent, counters never
  negative), pins (limit 5, four exact 422s), status counts
- **Polls:** create with status, vote (`choices[]`, four exact 422s,
  single-choice re-vote rejected, multiple add-a-choice), `hide_totals`
  masking, expiry (lazy + `PollExpired` job)
- **Quotes:** local auto-accept state machine, `quote` DTO (content only when
  accepted; shallow nesting one level down), `quotes_count`, revoke,
  quote list, `quote_approval_policy` + interaction-policy endpoint,
  `quote_approval {automatic, manual, current_user}`
- **Notifications:** records created by deduplicated `FanOutNotification`
  jobs committed with their interaction (read surface: Phase 4)
- **Media relations:** attachment metadata + order validated, `url:null`
  while unprocessed (bytes served in Phase 4)
- Schema **v3**: statuses/versions/pins/favourites/bookmarks/follows/blocks/
  domain-blocks/mutes/notes/endorsements/polls/quotes/notifications/
  idempotency/stats/media/mentions/hashtag tables + bounded composite indexes

Media, timelines, notifications, client usability (Phase 4):

- **Media:** v1 and v2 multipart upload (MIME sniffing → 422, description/focus),
  v2 video/audio `202` with a `ProcessMedia` job (`206` → `200`),
  `GET`/`PUT`/`PATCH`/`DELETE /api/v1/media/:id`, stored bytes served with the
  sniffed content type
- **Timelines:** home, public (`local`/`remote`/`only_media`), tag and list
  reads; exclusive `max_id`/`since_id`/`min_id` cursors, `Link` headers,
  `limit` clamped to 40; viewer-aware filtering; bounded index scans (sparse-filter
  test: 286 index entries walked, 13 read transactions for a page of 20)
- **Notifications:** index with `types[]`/`exclude_types[]`, show, clear,
  dismiss, `unread_count`, markers (`GET`/`POST /api/v1/markers`);
  `requests` always `[]`
- **Lists, filters, search, custom emoji, scheduled statuses:** see
  `docs/compatibility-profile.md` (Phase 4 section)
- **Client usability:** `scripts/smoke-phase4.sh` drives the workflow over HTTP
  with curl (not a third-party Mastodon client)

Tests:

- `cargo test` — **221 passed, 0 failed** (includes 23 `phase4_timelines`; the
  2026-10-17 domain-block correction adds two tests; evidence in
  `docs/logs/2026-10-17-domain-blocks-correction.md`)
- `cargo check --all-targets` — zero warnings
- `scripts/smoke-phase1.sh` → `SMOKE_OK`; `scripts/smoke-phase2.sh` →
  `SMOKE_PHASE2_OK`; `scripts/smoke-phase3.sh` → `SMOKE_PHASE3_OK`;
  `scripts/smoke-phase4.sh` → `SMOKE_PHASE4_OK`
- `bash scripts/smoke-all.sh` → `smoke-all: green`
- Full Phase 4 verification output: `docs/logs/2026-10-17-phase-4-verification.md`

## Phase 4.1 (parity gap closure, implemented 2026-10-17)

- JSON request bodies (`application/json`) on every REST write endpoint that
  reads a body; flattened into the form key model; malformed JSON answered as
  upstream does (D11). `POST /oauth/authorize` stays form-only (browser consent).
- Exclusive-list members are left out of home; the viewer's own statuses and
  list timelines are unaffected (D10b).
- Home reads the 1000 most recent follows; account cache 2000; the store's
  author cap is 1001. The home merge is now a lazy k-way merge, which fixed
  a silent drop of the oldest follows' posts that the 1000 cap exposed (D10a).
- `POST /api/v1/lists` accepts `replies_policy` and `exclusive`, as upstream does.
- Evidence: `docs/logs/2026-10-17-phase-4-1-verification.md`.

## Not yet implemented

- Phase 4 open items: see Known gaps below
- Phase 5 streaming; Phase 6 federation (incl. remote quote requests, remote
  statuses, and remote thread completion)
- Phase 7: account multi-fetch (`GET /api/v1/accounts`), account lookup/search,
  relation collection reads (`blocks`/`mutes`/`favourites`/`bookmarks`), tags, conversations, suggestions, announcements, donation
  campaigns, Web Push, first-party client (`/api/web/*`, keystodon bundle)
- Phase 7.1 media transcoding (GIF and video → WebM/VP9; stills → WebP); Phase 7.2 search engine;
  Phase 7.3 admin/moderation API, reports, trends, annual reports; Phase 8 metrics
- Every unimplemented route is explicitly absent (404), never a
  success-shaped stub — see `docs/endpoint-manifest.md`

## Known gaps

- Email-confirmation enforcement (no SMTP; `MailSender` is a logging adapter)
- Invite/approval registration modes; registration
  `locale`/`time_zone`/`date_of_birth` ignored
- Profile `discoverable`/`indexable`/`hide_collections`/`fields_attributes`
  accepted but not persisted
- Tag DTOs use the tag name as `id`; conversation mutes planned with conversations (Phase 7);
  `application` always shown (preference not stored)
- Phase 4 open items:
  - Cross-surface visibility parity is not one suite (per-surface coverage)
  - Wall time of page loaders is not measured (transaction/item counts only)
  - Home reads only the 1000 most recent follows (D10a, accepted; raised from 500 in Phase 4.1)
  - Tag feed excludes reblog wrappers, matching upstream (closed 2026-10-17)
  - `remote=true` is empty until federation (Phase 6)
  - No `206 Mastodon-Async-Refresh` regeneration for timelines (fan-out on read)
  - Notification requests always `[]`; notification-policy filter pipeline later
  - Account-payload emoji arrays are `[]` until Phase 7
  - Planned, not code gaps: transcoding (Phase 7.1), search engine (Phase 7.2),
    admin/moderation API (Phase 7.3)
- Details and the full deferral lists: `docs/phase-4-exit-criteria.md`,
  `docs/phase-3-exit-criteria.md` (plus `phase-2-exit-criteria.md`,
  `phase-1-exit-criteria.md`, and the still-open route-inventory review in
  `phase-0-exit-criteria.md`)
