# Feature matrix

Status: in progress (filled in as phases exit). Phase 1 (runtime/`redb`/IDs/
bootstrap) landed; no Mastodon REST endpoint is exposed yet, so user-facing rows
stay `planned` until their phase.

| Feature | Mastodon 4.7.3 | birchtree | Status |
|---|---|---|---|
| Account registration + OAuth (auth-code, PKCE S256, client-credentials) | yes | — | planned (Phase 1: account/user records, Argon2 credentials, OAuth app + token records, login session; endpoints Phase 2) |
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
