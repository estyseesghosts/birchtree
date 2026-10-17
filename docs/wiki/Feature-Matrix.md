# Feature matrix

Status: in progress (filled in as phases exit). Phases 1–3 (runtime/`redb`/
IDs/bootstrap, OAuth/registration/read APIs, local statuses/relationships/
interactions) landed; later-phase user-facing rows stay `planned` until their
phase.

| Feature | Mastodon 4.7.3 | birchtree | Status |
|---|---|---|---|
| Account registration + OAuth (auth-code, PKCE S256, client-credentials) | yes | partial | implemented (Phase 2: app registration, authorize/consent, token/revoke, registration policy open/closed, scopes; email confirmation not enforced, invite/approval modes pending) |
| Instance discovery + preferences | yes | partial | implemented (Phase 2: `/api/v1/instance`, `/api/v2/instance`, `/api/v1/preferences`) |
| Account read/update DTOs + relationships (read) | yes | partial | implemented (Phase 2 show/credentials; Phase 3 real relationship aggregates + counters; lookup/search and collections pending) |
| Statuses: create/read/edit/delete, source, context, history, multi-fetch | yes | yes | implemented (Phase 3, incl. `Idempotency-Key` replay and exact error shapes) |
| Visibility: public/unlisted/private/direct | yes | yes | implemented (Phase 3 policy matrix over real endpoints; feeds/search surfaces Phase 4+) |
| Quotes: state machine, policy, revocation, interaction policy | yes | yes | implemented (Phase 3 local only; remote quote requests Phase 6) |
| Relationships: follow (requests), block, mute, favourite, bookmark, pin/endorse, notes, domain blocks | yes | yes | implemented (Phase 3; collection reads `GET blocks/mutes/favourites/bookmarks` pending Phase 7) |
| Polls: create/vote/expiry (local) | yes | yes | implemented (Phase 3 create/vote via status + `/api/v1/polls/:id`; federation updates Phase 6) |
| Media attachment relations/order (metadata only) | yes | partial | implemented (Phase 3 relations + DTO shape, `url:null` while unprocessed; bytes/upload Phase 4) |
| Media v1/v2, lists, filters, search | yes | — | planned (Phase 7; media Phase 4) |
| Timelines: home/public/local/hashtag/list | yes | — | planned (Phase 4) |
| Notifications: records committed | yes | partial | implemented (Phase 3 creates deduplicated records via jobs; read/clear/dismiss Phase 4) |
| Streaming: SSE + WebSocket | yes | — | planned (Phase 5) |
| Federation: WebFinger, inbox, signed delivery, activities | yes | — | planned (Phase 6) |
| `redb` single-node storage | n/a (Mastodon uses PostgreSQL+Redis) | — | partial (Phase 1 schema/ports/atomicity/crash tests; Phase 3 schema v3 status/relationship/poll/quote tables) |
