# Feature matrix

Status: in progress (filled in as phases exit). Phases 1–4 and 4.1 (runtime/`redb`/IDs/bootstrap, OAuth/registration/read APIs, local statuses/relationships/interactions, media/timelines/notifications/client usability, parity gap closure) landed; later-phase user-facing rows stay `planned` until their phase. Rows added by the 2026-10-17 scope expansion are `planned` only.

| Feature | Mastodon 4.7.3 | birchtree | Status |
|---|---|---|---|
| Account registration + OAuth (auth-code, PKCE S256, client-credentials) | yes | partial | implemented (Phase 2: app registration, authorize/consent, token/revoke, registration policy open/closed, scopes; email confirmation not enforced, invite/approval modes pending) |
| Instance discovery + preferences | yes | partial | implemented (Phase 2: `/api/v1/instance`, `/api/v2/instance`, `/api/v1/preferences`) |
| Account read/update DTOs + relationships (read) | yes | partial | implemented (Phase 2 show/credentials; Phase 3 real relationship aggregates + counters; account multi-fetch, lookup and search planned Phase 7) |
| Statuses: create/read/edit/delete, source, context, history, multi-fetch | yes | yes | implemented (Phase 3, incl. `Idempotency-Key` replay and exact error shapes) |
| Visibility: public/unlisted/private/direct | yes | yes | implemented (Phase 3 policy matrix over real endpoints; applied to timelines in Phase 4; cross-surface parity suite open) |
| Quotes: state machine, policy, revocation, interaction policy | yes | yes | implemented (Phase 3 local only; remote quote requests Phase 6) |
| Relationships: follow (requests), block, mute, favourite, bookmark, pin/endorse, notes, domain blocks | yes | yes | implemented (Phase 3; domain blocks corrected to the upstream shape 2026-10-17; collection reads `GET blocks/mutes/favourites/bookmarks` pending Phase 7) |
| Polls: create/vote/expiry (local) | yes | yes | implemented (Phase 3 create/vote via status + `/api/v1/polls/:id`; federation updates Phase 6) |
| Media attachment relations/order (metadata only) | yes | partial | implemented (Phase 3 relations + DTO shape; Phase 4 upload, processing lifecycle, file serving; transcoding planned Phase 7.1) |
| Media v1/v2, lists, filters, search | yes | partial | implemented (Phase 4: media v1/v2 with processing lifecycle, lists, filters v1/v2 with `filtered` annotation, v2 search over the local corpus; transcoding planned Phase 7.1; search library planned Phase 7.2) |
| Timelines: home/public/local/hashtag/list | yes | partial | implemented (Phase 4; Phase 4.1: exclusive-list members hidden from home (D10b), home reads the 1000 most recent follows (D10a); open: `remote=true` empty until Phase 6; tag feed excludes reblog wrappers, matching upstream) |
| JSON request bodies on REST write endpoints | yes | yes | implemented (Phase 4.1, D11; `POST /oauth/authorize` consent form stays form-only; parity source-derived) |
| Notifications: records and read surface | yes | partial | implemented (Phase 3 deduplicated records via jobs; Phase 4 index, show, clear, dismiss, `unread_count`, markers; `requests` always `[]`) |
| Streaming: SSE + WebSocket | yes | — | planned (Phase 5) |
| Federation: WebFinger, inbox, signed delivery, activities | yes | — | planned (Phase 6) |
| Remote thread completion (ancestors at ingest and on logged-in context reads, deviation D5; descendants on authenticated context reads; crawl across hosts as upstream) | yes | — | planned (Phase 6; see `docs/federation-contract.md`) |
| Account multi-fetch `GET /api/v1/accounts?id[]`, lookup, search | yes | — | planned (Phase 7; multi-fetch up to 40 ids) |
| Conversations: threads, read/unread/delete, conversation mutes | yes | — | planned (Phase 7) |
| Suggestions: v1 (deprecated upstream) and v2 | yes | — | planned (Phase 7; FASP-sourced entries out of profile) |
| Announcements: list, dismiss, reactions | yes | — | planned (Phase 7; authored by CLI) |
| Donation campaigns | yes | — | planned (Phase 7; `204` unless the operator configures a campaign API) |
| Push notifications: Web Push subscriptions and delivery | yes | — | planned (Phase 7; VAPID keys from configuration) |
| First-party client: `/api/web/*` (embeds, settings, web push subscriptions), session login, CSRF, keystodon bundle | yes | — | planned (Phase 7; first-party mode, browser session + CSRF on `/api/web/*`) |
| Third-party client OAuth (keystodon as a client of any host) | yes | partial | implemented for birchtree as host (Phase 2 OAuth); keystodon-as-client interop tested when keystodon is available |
| Media transcoding: GIF and video → WebM/VP9 (video passthrough when already VP9 WebM within the size limit); still images → WebP; EXIF never stripped | yes (animated GIF → MP4 `gifv`; video transcoded) | — | planned (Phase 7.1; no `gifv` by decision; VP9 and WebP decided) |
| Search: status search (embedded library, on by default; admins can switch it off), account search (database) | yes (status search requires Elasticsearch) | partial | interim local corpus; embedded library planned (Phase 7.2); on by default is a recorded deviation (D6) |
| Admin API: `/api/v1/admin/*`, `/api/v2/admin/accounts` | yes | — | planned (Phase 7.3; roles `admin` and `moderator`, CLI-granted) |
| Moderation: reports (create, moderator queue) | yes | — | planned (Phase 7.3; remote Flag Phase 6) |
| Trends: public reads and admin approval | yes | — | planned (Phase 7.3) |
| Admin metrics: measures, dimensions, retention | yes | — | planned (Phase 7.3) |
| Annual reports | yes | — | planned (Phase 7.3) |
| Unmodified-client parity suite (identical client experience, the compatibility bar) | yes | — | planned (`birchtree.md` §8 item 11; run at each phase exit from Phase 3) |
| `redb` single-node storage | n/a (Mastodon uses PostgreSQL+Redis) | — | partial (Phase 1 schema/ports/atomicity/crash tests; Phase 3 schema v3 status/relationship/poll/quote tables) |
