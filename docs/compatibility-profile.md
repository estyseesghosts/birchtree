# docs/compatibility-profile.md

Status: Draft

Versioned API compatibility profile for the birchtree server, per `birchtree.md` §1 and §6.1.

## Target release

Mastodon **4.7.3** (API v7), commit `8ff11c2b`. The pinned tag and its changelog are the compatibility authority; `main`/unversioned docs are used only to detect later changes, never to silently change the target.

## Declared scope (Phase 0/1 baseline)

- **Local usable:** clients can register/authenticate, post/read/edit/delete, follow, interact, upload media, receive correct timelines/notifications — on a single server instance.
- **Federating:** not claimed in Phase 0/1. Two-server interop is a Phase 6 milestone.
- **Declared API compatibility:** not claimed by default. Every endpoint in the chosen public profile must have a passing contract test or a documented intentional exclusion before compatibility is advertised (Phase 7 exit gate).

## Implemented surface (Phase 2)

- **OAuth:** `POST /api/v1/apps`, `GET`/`POST /oauth/authorize`,
  `POST /oauth/token` (`authorization_code` + PKCE S256, `client_credentials`),
  `POST /oauth/revoke`. Tokens are non-expiring; `refresh_token` is
  `unsupported_grant_type`.
- **Registration/accounts:** `POST /api/v1/accounts` (app-only token required;
  open/closed policy), `GET /api/v1/accounts/{id}`,
  `GET /api/v1/accounts/verify_credentials`,
  `PATCH /api/v1/accounts/update_credentials`,
  `GET /api/v1/accounts/relationships`.
- **Discovery/settings:** `GET /api/v1/instance`, `GET /api/v2/instance`,
  `GET /api/v1/preferences`.
- **Statuses (Phase 3):** `GET /api/v1/statuses` (multi-fetch), create,
  show, edit (`PUT`/`PATCH`), delete, `source`, `context`, `history`,
  `interaction_policy`, favourite/unfavourite, bookmark/unbookmark,
  reblog/unreblog, pin/unpin, `favourited_by`, `reblogged_by`,
  `quotes` + quote revoke, `GET /api/v1/accounts/{id}/statuses` (incl.
  `?pinned=true`).
- **Relationships (Phase 3):** follow/unfollow, remove-from-followers,
  block/unblock, mute/unmute (`duration`, `notifications`), account notes,
  endorsements (`pin`/`endorse` + both list endpoints), follow requests
  (index/authorize/reject), domain blocks (index/create/destroy),
  followers/following lists, and real per-viewer
  `GET /api/v1/accounts/relationships`.
- **Polls (Phase 3):** `GET /api/v1/polls/{id}`, `POST /api/v1/polls/{id}/votes`.
- Not yet implemented (documented exclusions, see `phase-2-exit-criteria.md`
  and `phase-3-exit-criteria.md`): email-confirmation enforcement,
  invite/approval registration modes, account lookup/search, JSON request
  bodies, metrics.

## Implemented surface (Phase 4)

- **Media:** `POST /api/v1/media` (inline processing → 200),
  `POST /api/v2/media` (video/audio delayed → 202), `GET`/`PUT`/`PATCH`/
  `DELETE /api/v1/media/:id` (200/206/422/404 semantics), local file serving
  at `/system/media_attachments/files/...` with real `url`/`preview_url`/
  `meta`/`blurhash`.
- **Notifications:** index (with `types[]`/`exclude_types[]` + `Link`
  cursors), show, clear, dismiss, `unread_count` (marker-based), the
  always-honest `requests` surface; **markers** `GET`/`POST /api/v1/markers`.
- **Lists:** full CRUD + membership (`/api/v1/lists` and
  `/:id/accounts`), with the unfollow→membership cascade.
- **Filters:** v1 (synthesized from keyword rows) + full v2 CRUD with
  keyword/status sub-resources; payload `filtered` annotation matches every
  active filter like `CustomFilter.apply_cached_filters`.
- **Scheduled statuses:** `scheduled_at` on `POST /api/v1/statuses`
  (past → posts immediately; unparseable → 422 `Record invalid`; <5 min →
  pinned 422) + index/show/update/delete + durable publish job.
- **Search:** `GET /api/v2/search` over the local corpus (v1 does not exist
  in v4.7.3). **Custom emoji:** public `GET /api/v1/custom_emojis` +
  image serving + status-payload `emojis` arrays (CLI-managed).
- **Timelines:** `GET /api/v1/timelines/home` (user token; `read`/`read:statuses`;
  app-only → 422; no token → 401), `/public` (optional auth; `local`, `remote`,
  `only_media`), `/tag/:tag` (optional auth; unknown or empty tag → `200 []`),
  `/list/:id` (user token; `read`/`read:lists`; not owned or unparsable id →
  404 `{"error":"Record not found"}`).
  - Paging: `limit` absent → 20, otherwise min(|n|, 40); non-numeric or `0` →
    empty page with no `Link`. `max_id`/`since_id`/`min_id` are exclusive numeric
    cursors; non-numeric → 400 `Invalid cursor value`. `Link` `rel="next"` is the
    `max_id` of the oldest item and `rel="prev"` the `min_id` of the newest item,
    present only for a non-empty page.
  - Headers: anonymous public and tag → `Cache-Control: max-age=15, public,
    stale-while-revalidate=30, stale-if-error=86400`. Any authenticated timeline
    response, and home and list always, → `private, no-store`. This deviates from
    the plan's literal table: authenticated public bodies are viewer-specific.
  - Feed rules: public = public visibility only, self-replies kept, replies to
    others dropped, no reblogs. Tag = public visibility only, replies kept; reblog
    wrappers are not in the tag feed (open question, see below). Home = own
    statuses + active follows (most recent 500), direct only to mentioned viewers,
    private only from followed authors, reply rules by parent, `show_reblogs`
    respected. List = members' statuses, `replies_policy` (`list`/`followed`/
    `none`), `show_reblogs` respected. Blocks (both directions), mutes (viewer →
    author) and domain blocks apply to the viewer; anonymous viewers are unaffected.
  - Open / deviations: exclusive lists are not applied to home; home reads only
    the 500 most recent follows; tag-feed reblogs unsettled against v4.7.3;
    `remote=true` is empty until federation (Phase 6); no
    `206 Mastodon-Async-Refresh` regeneration (fan-out on read). Evidence:
    `tests/phase4_timelines.rs` (23) and `docs/phase-4-exit-criteria.md`.

## Exclusions (explicit, not implied gaps)

- Admin API, moderation dashboard, and full Mastodon web UI: explicitly out of scope until separately scoped.
- PostgreSQL, Redis, external broker: non-goals (`birchtree.md` §9).
- Multi-node / cluster mode: non-goal for the initial implementation.
- Full administrative compatibility in the public-client milestone; operator controls still required (CLI bootstrap, registration policy).
- React/Electron/separate client app/full frontend: non-goal. Minimal server-rendered login/consent only.

## Open decisions (to resolve in Phase 0, per `birchtree.md` §9)

- Supported signup/verification modes; local-only or closed-registration default.
- Exact target clients for client smoke tests.
- Whether push notifications and collections are in the declared profile.
- Policy for search indexing without Elasticsearch.
- Maximum status/media limits.
- Whether public web/ActivityPub object routes are part of the first federation milestone.
- Latency/resource acceptance thresholds.

## Reporting rule

Never report full v4.7.3 compatibility by default. Report exact scope: local only, federation subset, or declared public API. Remaining exclusions must be published and obvious.
