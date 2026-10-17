# docs/compatibility-profile.md

Status: Draft

Versioned API compatibility profile for the birchtree server, per `birchtree.md` §1 and §6.1.

## Target release

Mastodon **4.7.3** (API v7), commit `8ff11c2b`. The pinned tag and its changelog are the compatibility authority; `main`/unversioned docs are used only to detect later changes, never to silently change the target.

## Compatibility bar

An unmodified Mastodon client connects to a birchtree instance and has an identical experience to connecting to Mastodon v4.7.3, measured at 98% or better (`birchtree.md` §1). The core flows (sign-in, compose, timelines, notifications, media) must match in full (proposed). The bar is checked by the unmodified-client parity suite (`birchtree.md` §8, item 11). Every difference the suite can see must appear in the deviation register below, with its reason and status, whatever the overall percentage. A difference that is not in the register is a bug.

## Declared scope (Phase 0/1 baseline)

- **Local usable:** clients can register/authenticate, post/read/edit/delete, follow, interact, upload media, receive correct timelines/notifications — on a single server instance.
- **Federating:** not claimed in Phase 0/1. Two-server interop is a Phase 6 milestone.
- **Declared API compatibility:** not claimed by default. Every endpoint in the chosen public profile must have a passing contract test or a documented intentional exclusion before compatibility is advertised (the Phase 7 and 7.1–7.3 exit gates, per `birchtree.md` §7).

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
  (index/authorize/reject), domain blocks (index, `POST` create, `DELETE`
  destroy, in the upstream shape since 2026-10-17; see `docs/endpoint-manifest.md`),
  followers/following lists, and real per-viewer
  `GET /api/v1/accounts/relationships`.
- **Polls (Phase 3):** `GET /api/v1/polls/{id}`, `POST /api/v1/polls/{id}/votes`.
- Not yet implemented (documented exclusions, see `phase-2-exit-criteria.md`
  and `phase-3-exit-criteria.md`): email-confirmation enforcement,
  invite/approval registration modes, account lookup/search, metrics. JSON
  request bodies were a gap here and were closed in Phase 4.1 (D11).

## Implemented surface (Phase 4)

- **Media:** `POST /api/v1/media` (inline processing → 200),
  `POST /api/v2/media` (video/audio delayed → 202), `GET`/`PUT`/`PATCH`/
  `DELETE /api/v1/media/:id` (200/206/422/404 semantics), local file serving
  at `/system/media_attachments/files/...` with real `url`/`preview_url`/
  `meta`/`blurhash`. Interim: no transcoding toolchain, so animated GIF stays
  `image` (no `gifv`) and video/audio bytes are untouched. Transcoding is
  planned as Phase 7.1 (animated GIF and video → WebM/VP9; the `gifv` omission is
  a recorded deviation in `docs/endpoint-manifest.md`).
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
- **Search (interim):** `GET /api/v2/search` over the local corpus until the
  Phase 7.2 search library lands (v1 does not exist in v4.7.3). **Custom emoji:** public
  `GET /api/v1/custom_emojis` + image serving + status-payload `emojis` arrays
  (CLI-managed).
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
    statuses + active follows (most recent 1000, from Phase 4.1), direct only to mentioned viewers,
    private only from followed authors, reply rules by parent, `show_reblogs`
    respected. List = members' statuses, `replies_policy` (`list`/`followed`/
    `none`), `show_reblogs` respected. Blocks (both directions), mutes (viewer →
    author) and domain blocks apply to the viewer; anonymous viewers are unaffected.
  - Phase 4.1 (implemented 2026-10-17): exclusive-list members are hidden from home (D10b); home reads only
    the 1000 most recent follows (D10a, raised from 500); tag-feed reblogs match upstream (verified 2026-10-17);
    `remote=true` is empty until federation (Phase 6); no
    `206 Mastodon-Async-Refresh` regeneration (fan-out on read). Evidence:
    `tests/phase4_timelines.rs` (23) and `docs/phase-4-exit-criteria.md`.

## Planned in-profile surface (not implemented)

Every group below is in the declared profile and has a planned phase. Each route is a row in `docs/endpoint-manifest.md`. Until its phase ships, the route is absent (404), never a success-shaped stub.

- **Phase 4.1 (parity gap closure, before Phase 5): implemented 2026-10-17.** JSON request bodies on every write endpoint that reads a body (D11; `POST /oauth/authorize` stays form-only); exclusive lists hidden from home (D10b); home cap 1000 (D10a). Details: `docs/endpoint-manifest.md`, "Phase 4.1: JSON bodies and exclusive lists".
- **Phase 6 (federation):** remote thread completion, which adds `Mastodon-Async-Refresh` to authenticated `GET /api/v1/statuses/:id/context` reads: descendants (the crawl follows replies on any host, as upstream does) and ancestors for logged-in viewers (D5); Flag send and receive; WebFinger, NodeInfo, actor and key documents. Remote `timelines/public?remote=true` and `resolve` on `GET /api/v2/search` also depend on this phase.
- **Phase 7 (client-facing parity):** account multi-fetch (`GET /api/v1/accounts?id[]=…`, up to 40 ids), lookup and search; conversations (4 routes) and conversation mutes (2); suggestions (`/api/v1/suggestions`, `/api/v2/suggestions`); announcements (list, dismiss, reactions); donation campaigns; Web Push subscriptions (5 routes) and delivery; the first-party client: `/api/web/*` (7 routes, first-party session and CSRF) and serving keystodon's built bundle. Third-party clients use OAuth only.
- **Phase 7.1 (media transcoding):** GIF → WebM with VP9 (typed `video`, `gifv` never produced); video → WebM with VP9 (passthrough when already VP9 WebM within the size limit); still images → WebP (passthrough when already WebP); the server never strips EXIF.
- **Phase 7.2 (search library):** an embedded full-text library behind `GET /api/v2/search`, selectable as `embedded` (default), `database` or `off`. The library choice is open at scoping.
- **Phase 7.3 (administration, moderation, trends, analytics):** admin API (62 `/api/v1/admin/*` routes and `GET /api/v2/admin/accounts`), `POST /api/v1/reports`, public trends (`/api/v1/trends`, `/tags`, `/links`, `/statuses`), annual reports (5 routes). The admin API needs the `admin` and `moderator` roles, which are decided and CLI-granted; the permission split between them is proposed.

## Deviation register

Each row is a difference from Mastodon v4.7.3 that a client can observe, or a gap that breaks the compatibility bar. Status values: **accepted** (operator decision recorded), **decided** (planned design, operator-approved, not yet built), **recommended** (my proposal, waiting for an operator decision), **open** (needs an operator decision), **gap** (not yet built; the bar is not met until it is), **closed** (verified to match upstream; no deviation remains).

| ID | Difference | Upstream v4.7.3 | Effect on the client | Status |
|---|---|---|---|---|
| D1 | `gifv` is never produced | Animated GIF → MP4 with `type: gifv` (silent video also gets `gifv`, `lib/paperclip/transcoder.rb`) | A client that applies `gifv` autoplay and loop shows the attachment as an ordinary video | accepted (operator, 2026-10-17; intentional) |
| D2 | GIF (animated or not) → WebM with VP9, typed `video` (Phase 7.1) | GIF → MP4 (`lib/paperclip/gif_transcoder.rb`) | Container, codec, bytes and `url` differ; same visibility as D1 | accepted (operator, 2026-10-17) |
| D3 | Video → WebM with VP9 (Phase 7.1); a file already in VP9 WebM within the upload size limit passes through | Video → MP4 (`lib/paperclip/transcoder.rb`), with passthrough for compatible files | Bytes, `url` and `meta` differ; playability depends on client WebM/VP9 support | accepted (operator, 2026-10-17) |
| D4 | Still images → WebP (Phase 7.1); an image already in WebP passes through; the server never strips or rewrites EXIF | Originals kept; only HEIC, HEIF and AVIF converted to JPEG | Image bytes and `url` differ for every converted image; metadata carried through | accepted (operator, 2026-10-17) |
| D5 | Read-time ancestor completion for logged-in viewers (Phase 6) | Missing ancestors are not fetched on read | A logged-in context can show ancestors that Mastodon would not | accepted (operator, 2026-10-17; intentional) |
| D6 | Status search is on by default, `embedded` (Phase 7.2); an administrator can switch it off | Status search returns no hits when Elasticsearch is disabled (the default install) | With the default configuration, status search returns hits that Mastodon's default install does not | accepted (operator, 2026-10-17): on by default; admins can disable it |
| D7 | Status search `database` mode is a bounded local scan (Phase 4 interim) | As D6 | As D6 | interim, until Phase 7.2 |
| D8 | Authenticated public and tag timelines send `Cache-Control: private, no-store` (Phase 4) | Public `max-age=15` header on every public and tag response | Shared caches behave differently; the body is the same | accepted (Phase 4) |
| D9 | No `206 Mastodon-Async-Refresh` for the home feed; the feed is served complete (Phase 4) | Home feed returns `206` with `Mastodon-Async-Refresh` (retry 5) while it regenerates | A first load is complete at once; Mastodon's may be partial with a retry. Only the status code and header timing differ | accepted (operator, 2026-10-17); revisit only if a client breaks |
| D10a | Home reads the 1000 most recent follows (Phase 4.1; was 500) | Home is built from every follow; the feed holds the most recent posts overall | Only accounts that follow more than 1000 accounts can miss older follows' posts | accepted (operator, 2026-10-17); implemented in Phase 4.1 |
| D10b | Exclusive lists are hidden from home, as upstream does (Phase 4.1) | Members of exclusive lists are hidden from home (`FeedManager#filter_from_home`, `:skip_home`) | None | implemented (2026-10-17, Phase 4.1); evidence `tests/phase4_1_exclusive_home.rs` |
| D10c | Tag feed excludes reblog wrappers (Phase 4) | Reblog rows have no tag rows (`ReblogService`), so the tag feed excludes them too | None | closed (verified 2026-10-17; matches upstream) |
| D11 | JSON request bodies on REST write endpoints (Phase 4.1) | JSON parameter bodies accepted (Rails default parsing) | None for the REST write endpoints. `POST /oauth/authorize` (browser consent) stays form-only: a JSON body answers `415` | implemented (2026-10-17, Phase 4.1); parity source-derived, see `docs/logs/2026-10-17-phase-4-1-verification.md` |
| D12 | Email confirmation is not enforced; invite and approval registration modes are missing (Phase 2) | Enforced when configured | Registration flow differs | gap (Phase 2 open items) |
| D13 | `Link` headers echo the whole query minus the cursors (Phase 4) | Echo only the permitted parameters | A client that follows `Link` gets the same page | accepted (Phase 4) |

**Decisions (2026-10-17)**

- **D3 (video):** VP9 WebM with Opus audio. A video already in VP9 WebM within the upload size limit passes through unchanged. Other videos are transcoded to WebM/VP9.
- **D4 (still images):** WebP. An image already in WebP passes through. The server never strips or rewrites EXIF; the client handles privacy.
- **D9 (home `206`):** accepted. Revisit only if a client breaks.
- **D10a (follow cap):** 1000, accepted as a deviation; implemented in Phase 4.1.
- **D10b (exclusive lists):** match upstream; implemented in Phase 4.1.
- **D11 (JSON bodies):** Phase 4.1, implemented.

## Exclusions (explicit, not implied gaps)

- **Admin dashboard and Mastodon web app:** the server-rendered admin dashboard (`config/routes/admin.rb`) and the Mastodon web app (`config/routes/web_app.rb`, `/web/*`) are out of profile. Admin **API** compatibility is in profile (Phase 7.3).
- **Absent upstream:** `GET /api/v1/search` (removed in Mastodon 3.0; only `GET /api/v2/search` exists at v4.7.3).
- **No provider:** `POST /api/v1/statuses/:id/translate` (translation provider not configured).
- **Experimental/legacy:** `namespace :v1_alpha` (collections, async refreshes) and `GET /api/v1/oembed` (not in the initial profile).
- **Deliberate deviation, not an exclusion:** `gifv` is never produced. Animated GIF becomes WebM/VP9 typed `video` (Phase 7.1).
- PostgreSQL, Redis, external broker: non-goals (`birchtree.md` §9).
- Multi-node / cluster mode: non-goal for the initial implementation.
- No frontend source in this repository. birchtree serves keystodon's built first-party bundle (Phase 7). keystodon is developed in a separate project. Server-rendered pages are limited to login, consent and the first-party login.

## Open decisions (each resolved in the phase named, per `birchtree.md` §9)

- Supported signup/verification modes; local-only or closed-registration default (Phase 2 gaps; needed by the Phase 7.3 approve/reject routes).
- Exact target clients: keystodon, in both its first-party and third-party modes, once it is available on the development machine. Client smoke tests wait for it.
- Whether collections are in the declared profile. Push notifications are in profile (Phase 7).
- The embedded search library (Phase 7.2): candidates to evaluate for license, maintenance and fit.
- Phase 7.1 implementation settings: WebP and VP9 quality; `meta` for converted blobs. Decided: derived blobs replace originals after commit; v1 transcodes inline (`200`), v2 returns `202` for transcoded types.
- keystodon's first-party mode (Phase 7): bundle mount path and artifact source; whether first-party `/api/v1/*` calls use OAuth tokens (proposed default) or the session.
- Admin permission split between `admin` and `moderator` (proposed: moderator covers reports and account actions; admin covers every admin route), and the audit trail (proposed: on) (Phase 7.3).
- Search is on by default (D6, decided); the administrator switch is built in Phase 7.2 and 7.3.
- Maximum status/media limits.
- Whether public web/ActivityPub object routes are part of the first federation milestone (Phase 6).
- Latency/resource acceptance thresholds (Phase 8).

## Reporting rule

Never report full v4.7.3 compatibility by default. Report exact scope: local only, federation subset, or declared public API. Remaining exclusions must be published and obvious.
