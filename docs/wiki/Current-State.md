# Current state

Status: 2026-10-17, Phase 3 (local statuses, relationships, interaction
semantics) implemented; Phase 4 next.

## Implemented

Repository and contracts:

- Repository layout per `birchtree.md` §3 (one Cargo package, `domain` ← `application` ← adapters)
- `AGENTS.md`, `docs/` contracts, `docs/wiki/`, append-only `state/logs/` and `docs/logs/`
- `scripts/smoke-all.sh` full gate (check + test + phase1/2/3 smokes);
  `.github/workflows/ci.yml` release-build CI

Runtime (Phase 1):

- Structured `tracing` logging with `RUST_LOG` filtering and secret redaction
- `GET /health` and `GET /ready` probes; unknown routes return 404
- Graceful shutdown on SIGINT/SIGTERM; configuration validation; data-dir
  path permissions (`0700`); body limits; trusted-proxy client-IP handling
- CLI: `serve`, `create-account` (plus test-only `crash-harness`)
- Mastodon-shaped snowflake IDs (numeric ordering, decimal-string wire) and a
  monotonic `Clock`
- Versioned `redb` schema + migration runner (now at **v3**), narrow
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
  endorsement cleanup), domain blocks, mutes with duration/notifications,
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
  jobs committed with their interaction (read surface Phase 4)
- **Media relations:** attachment metadata + order validated, `url:null`
  while unprocessed (bytes Phase 4)
- Schema **v3**: statuses/versions/pins/favourites/bookmarks/follows/blocks/
  domain-blocks/mutes/notes/endorsements/polls/quotes/notifications/
  idempotency/stats/media/mentions/hashtag tables + bounded composite indexes

Tests:

- `cargo test` — **138 passed, 0 failed** (51 unit + 3 `crash_recovery` + 18
  `persistence_conformance` + 13 `phase2_api` + 13 `phase2_oauth` + 8
  `phase3_interactions` + 13 `phase3_relationships` + 13 `phase3_status` + 3
  `runtime` + 3 `secret_handling`)
- `scripts/smoke-phase1.sh` → `SMOKE_OK`; `scripts/smoke-phase2.sh` →
  `SMOKE_PHASE2_OK`; `scripts/smoke-phase3.sh` → `SMOKE_PHASE3_OK`
- `bash scripts/smoke-all.sh` → `smoke-all: green`

## Not yet implemented

- Phase 4: media bytes/upload/serving, timelines, notification read surface
  (records exist), `StatusViewContext` page-loader measurements
- Phase 5 streaming; Phase 6 federation (incl. remote quote requests)
- Phase 7: account lookup/search, relation collection reads
  (`blocks`/`mutes`/`favourites`/`bookmarks`), JSON request bodies, lists/
  filters/search/tags; Phase 8 metrics
- Every unimplemented route is explicitly absent (404), never a
  success-shaped stub — see `docs/endpoint-manifest.md`

## Known gaps

- Email-confirmation enforcement (no SMTP; `MailSender` is a logging adapter)
- Invite/approval registration modes; registration
  `locale`/`time_zone`/`date_of_birth` ignored
- Profile `discoverable`/`indexable`/`hide_collections`/`fields_attributes`
  accepted but not persisted
- Tag DTOs use the tag name as `id`; conversation mutes out of profile;
  `application` always shown (preference not stored)
- Details and the full deferral list: `docs/phase-3-exit-criteria.md`
  (plus `phase-2-exit-criteria.md`, `phase-1-exit-criteria.md`, and the
  still-open route-inventory review in `phase-0-exit-criteria.md`)
