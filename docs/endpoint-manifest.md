# docs/endpoint-manifest.md

Status: Draft

Route inventory from Mastodon v4.7.3 `config/routes/api.rb` (commit `8ff11c2b`), cross-checked against the public API docs where practical. One manifest row per method/path. Profile column: `In` = in the declared Phase 0/1+ compatibility profile, `Out` = explicitly excluded (admin/moderation, legacy, experimental, or not yet scoped), `Plan` = planned for a later phase.

Auth scopes are Mastodon conventions from the source/docs; they are enforced centrally in Phase 2+, not yet implemented here.

## OAuth / discovery (public, not in config/routes/api.rb)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/.well-known/webfinger` | none | — | planned | Plan | WebFinger `acct:` resolution (Phase 6) |
| GET | `/.well-known/nodeinfo` | none | — | planned | Plan | NodeInfo discovery (Phase 6) |
| GET | `/nodeinfo/2.0` | none | — | planned | Plan | NodeInfo 2.0 document (Phase 6) |
| GET/POST | `/oauth/authorize` | session | — | planned | Plan | Doorkeeper authorize; consent page rendered server-side |
| POST | `/oauth/token` | basic/client | — | planned | Plan | Token issue (auth-code + client-credentials) |
| POST | `/oauth/revoke` | basic/client | — | planned | Plan | Token revocation |

## v1 REST API (from `config/routes/api.rb`)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/instance` | optional | read | stubbed | In | instance discovery needed by clients |
| GET | `/api/v2/instance` | optional | read | stubbed | In | instance discovery v2 |
| GET | `/api/v1/accounts/verify_credentials` | user | read | stubbed | In | |
| PATCH | `/api/v1/accounts/update_credentials` | user | write | stubbed | In | incl. `source[quote_policy]` |
| GET | `/api/v1/accounts/lookup` | user | read | stubbed | In | acct → account resolution |
| GET | `/api/v1/accounts/search` | user | read | stubbed | In | |
| GET | `/api/v1/accounts/relationships` | user | read | stubbed | In | |
| GET | `/api/v1/accounts/:id` | optional | read | stubbed | In | |
| GET | `/api/v1/accounts/:id/statuses` | optional | read | stubbed | In | |
| GET | `/api/v1/accounts/:id/followers` | optional | read | stubbed | In | |
| GET | `/api/v1/accounts/:id/following` | optional | read | stubbed | In | |
| POST | `/api/v1/accounts/:id/follow` | user | follow | stubbed | In | |
| POST | `/api/v1/accounts/:id/unfollow` | user | follow | stubbed | In | |
| POST | `/api/v1/accounts/:id/block` | user | write | stubbed | In | |
| POST | `/api/v1/accounts/:id/unblock` | user | write | stubbed | In | |
| POST | `/api/v1/accounts/:id/mute` | user | write | stubbed | In | |
| POST | `/api/v1/accounts/:id/unmute` | user | write | stubbed | In | |
| POST | `/api/v1/accounts/:id/remove_from_followers` | user | write | stubbed | In | |
| POST | `/api/v1/apps` | user | — | stubbed | In | app registration; also client-credentials source |
| GET | `/api/v1/apps/verify_credentials` | app | — | stubbed | In | |
| GET | `/api/v1/statuses/:id` | optional | read | stubbed | In | |
| POST | `/api/v1/statuses` | user | write | stubbed | In | incl. `quoted_status_id`/`quote_approval_policy` |
| PUT | `/api/v1/statuses/:id` | user | write | stubbed | In | edit (history + follow-up jobs) |
| DELETE | `/api/v1/statuses/:id` | user | write | stubbed | In | |
| GET | `/api/v1/statuses/:id/source` | optional | read | stubbed | In | source/raw-text mode |
| GET | `/api/v1/statuses/:id/context` | optional | read | stubbed | In | ancestors/descendants |
| GET | `/api/v1/statuses/:id/history` | optional | read | stubbed | In | edit history |
| PUT | `/api/v1/statuses/:id/interaction_policy` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/favourite` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/unfavourite` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/bookmark` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/unbookmark` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/reblog` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/unreblog` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/mute` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/unmute` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/pin` | user | write | stubbed | In | |
| POST | `/api/v1/statuses/:id/unpin` | user | write | stubbed | In | |
| GET | `/api/v1/statuses/:id/quotes` | optional | read | stubbed | In | quote listing |
| POST | `/api/v1/statuses/:id/quotes/:quoting_status_id/revoke` | user | write | stubbed | In | quote revocation |
| GET | `/api/v1/timelines/home` | user | read | stubbed | In | |
| GET | `/api/v1/timelines/public` | optional | read | stubbed | In | |
| GET | `/api/v1/timelines/tag` | optional | read | stubbed | In | |
| GET | `/api/v1/timelines/list` | user | read | stubbed | In | |
| GET | `/api/v1/timelines/link` | optional | read | stubbed | In | |
| GET | `/api/v1/media/:id` | optional | read | stubbed | In | |
| POST | `/api/v1/media` | user | write | stubbed | In | media v1 (multipart) |
| PUT | `/api/v1/media/:id` | user | write | stubbed | In | |
| DELETE | `/api/v1/media/:id` | user | write | stubbed | In | |
| POST | `/api/v2/media` | user | write | stubbed | In | media v2 (multipart) |
| GET | `/api/v1/notifications` | user | read:notifications | stubbed | In | |
| GET | `/api/v1/notifications/:id` | user | read:notifications | stubbed | In | |
| POST | `/api/v1/notifications/clear` | user | write | stubbed | In | |
| GET | `/api/v1/notifications/unread_count` | user | read:notifications | stubbed | In | |
| POST | `/api/v1/notifications/:id/dismiss` | user | write | stubbed | In | |
| GET | `/api/v1/preferences` | user | read | stubbed | In | incl. quote-policy defaults |
| GET | `/api/v1/filters` | user | read | stubbed | Plan | Phase 7 |
| POST | `/api/v1/filters` | user | write | stubbed | Plan | Phase 7 |
| GET | `/api/v1/filters/:id` | user | read | stubbed | Plan | Phase 7 |
| PUT | `/api/v1/filters/:id` | user | write | stubbed | Plan | Phase 7 |
| DELETE | `/api/v1/filters/:id` | user | write | stubbed | Plan | Phase 7 |
| GET | `/api/v1/lists` | user | read | stubbed | Plan | Phase 7 |
| POST | `/api/v1/lists` | user | write | stubbed | Plan | Phase 7 |
| GET | `/api/v1/lists/:id` | user | read | stubbed | Plan | Phase 7 |
| PUT | `/api/v1/lists/:id` | user | write | stubbed | Plan | Phase 7 |
| DELETE | `/api/v1/lists/:id` | user | write | stubbed | Plan | Phase 7 |
| GET | `/api/v1/polls/:id` | optional | read | stubbed | Plan | Phase 7 |
| POST | `/api/v1/polls/:id/votes` | user | write | stubbed | Plan | Phase 7 |
| GET | `/api/v1/scheduled_statuses` | user | read | stubbed | Plan | Phase 7 |
| GET | `/api/v1/search` | user | read | stubbed | Plan | Phase 7 |
| GET | `/api/v2/search` | user | read | stubbed | Plan | Phase 7 |
| GET | `/api/v1/streaming` | user | read | stubbed | Plan | Phase 5 (SSE/WebSocket) |
| GET | `/api/v1/streaming/(*any)` | user | read | stubbed | Plan | Phase 5 |

### Explicitly excluded / out of profile (admin, moderation, legacy, experimental)

- `GET /api/v1/oembed` — public oEmbed; not in initial profile (record as gap if clients need it).
- `namespace :v1_alpha` — experimental/legacy (`accounts/collections`, `collections/*`); explicitly **Out**.
- `annual_reports`, `announcements`, `conversations`, `donation_campaigns`, `suggestions`, `custom_emojis` — **Out** until scoped (custom emoji may be needed for account/status serialization; revisit).
- `GET /api/v1/blocks`, `GET /api/v1/mutes`, `GET /api/v1/favourites`, `GET /api/v1/bookmarks`, `GET /api/v1/endorsements` — relation collection reads; **Plan** (Phase 7).
- `POST /api/v1/reports` — moderation; **Out** (admin/moderation excluded).
- `follow_requests` index/authorize/reject — **Plan** (locked accounts; Phase 3+).
- `notifications/requests` (follow-request notifications) — **Plan** (Phase 3+).
- `accounts/familiar_followers`, `identity_proofs`, `featured_tags`, `email_subscriptions`, `collections`, `tags` follow/feature, `followed_tags` — **Plan** (Phase 7).
- `namespace :admin` (all `admin/*` routes) — **Out** explicitly; moderation/security operations excluded from public-client milestone.
- `v2` admin accounts, `push/*`, `web/*` — **Out** unless scoped later.
- `translate` (statuses) — **Out** unless a translation provider is configured.

## Non-route contract items tracked separately

- IDs: decimal strings on wire, numeric ordering internally (§4 of `birchtree.md`).
- Pagination: `Link` `next`/`prev` headers; cursor bounds inclusive/exclusive per endpoint.
- Error shapes: `{"error": "..."}`; statuses 202/204/206/401/403/404/422 where applicable.
- Rate-limit headers, idempotency (`Idempotency-Key`, 1-hour retention), streaming event envelopes.
