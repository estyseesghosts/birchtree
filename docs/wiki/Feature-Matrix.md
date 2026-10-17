# Feature matrix

Status: in progress (filled in as phases exit). Phases 1–2 (runtime/`redb`/IDs/
bootstrap + OAuth, registration and read APIs) landed; later-phase user-facing
rows stay `planned` until their phase.

| Feature | Mastodon 4.7.3 | birchtree | Status |
|---|---|---|---|
| Account registration + OAuth (auth-code, PKCE S256, client-credentials) | yes | partial | implemented (Phase 2: app registration, authorize/consent, token/revoke, registration policy open/closed, scopes; email confirmation not enforced, invite/approval modes pending) |
| Instance discovery + preferences | yes | partial | implemented (Phase 2: `/api/v1/instance`, `/api/v2/instance`, `/api/v1/preferences`) |
| Account read/update DTOs + relationships (read) | yes | partial | implemented (Phase 2: show, verify/update credentials, relationships; lookup/search and collections pending) |
| Statuses: create/read/edit/delete, source, context, history | yes | — | planned |
| Visibility: public/unlisted/private/direct | yes | — | planned |
| Quotes: state machine, policy, revocation | yes | — | planned |
| Relationships: follow/block/mute/favourite/bookmark | yes | — | planned |
| Polls, media v1/v2, lists, filters, search | yes | — | planned |
| Timelines: home/public/local/hashtag/list | yes | — | planned |
| Notifications: read/clear/dismiss | yes | — | planned |
| Streaming: SSE + WebSocket | yes | — | planned |
| Federation: WebFinger, inbox, signed delivery, activities | yes | — | planned |
| `redb` single-node storage | n/a (Mastodon uses PostgreSQL+Redis) | — | partial (Phase 1: versioned schema, migrations, narrow ports, atomic mutation+job commit, crash-recovery) |
