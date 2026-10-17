# docs/endpoint-manifest.md

Status: Draft

Route inventory from Mastodon v4.7.3 `config/routes/api.rb` (commit `8ff11c2b`), cross-checked against the public API docs where practical. One manifest row per method/path. Profile column: `In` = in the declared compatibility profile (a row whose phase is still `planned` is `In`); `Out` = explicitly excluded (absent at v4.7.3, experimental or legacy, or no provider configured). Status column: `implemented (Phase N)` = route exists with contract tests; `planned (Phase N)` = in profile but intentionally absent (404) until that phase — never a success-shaped stub.

Scope expansion (2026-10-17): the admin/moderation API, reports, Web Push, conversations, suggestions, announcements, trends, annual reports, donation campaigns, and the `/api/web/*` routes are `In` with planned phases (see `birchtree.md` §7). Their rows are in the sections below. Known inventory gaps and one route-shape discrepancy are listed at the end.

Auth scopes are Mastodon conventions from the source/docs. They are enforced centrally (Phase 2) at the route boundary: a missing/invalid token yields `401 {"error":"The access token is invalid"}`, an insufficient scope yields `403 {"error":"This action is outside the authorized scopes"}`, and a user endpoint reached with an app-only token yields `422 {"error":"This method requires an authenticated user"}` (or `403 {"error":"This method requires an client credentials authentication"}` for the reverse case on registration). Scope lists are "any-of" (broad OR granular), matching Doorkeeper's `any?`. Admin routes additionally require a moderator-or-admin role (Mastodon's `Admin::PermissionsConcern`); the role model is Phase 7.3 work.

## OAuth / discovery (public, not in config/routes/api.rb)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/.well-known/webfinger` | none | — | planned (Phase 6) | In | WebFinger `acct:` resolution |
| GET | `/.well-known/nodeinfo` | none | — | planned (Phase 6) | In | NodeInfo discovery |
| GET | `/nodeinfo/2.0` | none | — | planned (Phase 6) | In | NodeInfo 2.0 document |
| GET/POST | `/oauth/authorize` | session | — | implemented (Phase 2) | In | Doorkeeper authorize; login + consent rendered server-side |
| POST | `/oauth/token` | basic/client | — | implemented (Phase 2) | In | `authorization_code` (PKCE S256) + `client_credentials` |
| POST | `/oauth/revoke` | basic/client | — | implemented (Phase 2) | In | Always `200 {}`; wrong client `403 unauthorized_client` |

## v1 REST API (from `config/routes/api.rb`)

### Instances, applications, accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/instance` | none | — | implemented (Phase 2) | In | instance discovery, `REST::V1::InstanceSerializer` |
| GET | `/api/v2/instance` | none | — | implemented (Phase 2) | In | instance discovery v2, `REST::InstanceSerializer` |
| GET | `/api/v1/accounts` | optional | read \| read:accounts | planned (Phase 7) | In | multi-fetch `id[]` (up to 40; above that `422 Mastodon::ValidationError`); unapproved and pending-deletion accounts omitted (upstream `set_accounts`) |
| POST | `/api/v1/accounts` | app | write \| write:accounts | implemented (Phase 2) | In | registration; returns a user token |
| GET | `/api/v1/accounts/verify_credentials` | user | profile \| read \| read:accounts | implemented (Phase 2) | In | `CredentialAccountSerializer` (real counters) |
| PATCH | `/api/v1/accounts/update_credentials` | user | write \| write:accounts | implemented (Phase 2) | In | incl. `source[quote_policy]` |
| GET | `/api/v1/accounts/lookup` | user | read | planned (Phase 7) | In | acct → account resolution |
| GET | `/api/v1/accounts/search` | user | read | planned (Phase 7) | In | |
| GET | `/api/v1/accounts/relationships` | user | read \| read:follows | implemented (Phase 2, real data Phase 3) | In | real per-viewer relationship aggregates |
| GET | `/api/v1/accounts/:id` | none | — | implemented (Phase 2, real counters Phase 3) | In | public profile |
| GET | `/api/v1/accounts/:id/statuses` | optional | read | implemented (Phase 3) | In | `pinned`/`exclude_replies`/`exclude_reblogs` filters, `Link` cursors |
| GET | `/api/v1/accounts/:id/followers` | optional | read | implemented (Phase 3) | In | `REST::AccountSerializer[]`, `Link` cursors |
| GET | `/api/v1/accounts/:id/following` | optional | read | implemented (Phase 3) | In | |
| POST | `/api/v1/accounts/:id/follow` | user | follow \| write \| write:follows | implemented (Phase 3) | In | locked target → `requested:true` |
| POST | `/api/v1/accounts/:id/unfollow` | user | follow \| write \| write:follows | implemented (Phase 3) | In | also cancels pending requests; drops endorsements |
| POST | `/api/v1/accounts/:id/block` | user | follow \| write \| write:blocks | implemented (Phase 3) | In | mutual unfollow + endorsement cleanup |
| POST | `/api/v1/accounts/:id/unblock` | user | follow \| write \| write:blocks | implemented (Phase 3) | In | |
| POST | `/api/v1/accounts/:id/mute` | user | follow \| write \| write:mutes | implemented (Phase 3) | In | `duration` seconds (0 = permanent), `notifications` |
| POST | `/api/v1/accounts/:id/unmute` | user | follow \| write \| write:mutes | implemented (Phase 3) | In | |
| POST | `/api/v1/accounts/:id/remove_from_followers` | user | follow \| write \| write:follows | implemented (Phase 3) | In | idempotent 200 |
| POST | `/api/v1/accounts/:id/note` | user | write \| write:accounts | implemented (Phase 3) | In | `comment` ≤2000; blank deletes |
| POST | `/api/v1/accounts/:id/pin` | user | write \| write:accounts | implemented (Phase 3) | In | endorsement (alias `/endorse`); requires active follow |
| POST | `/api/v1/accounts/:id/endorse` | user | write \| write:accounts | implemented (Phase 3) | In | alias of `pin` |
| POST | `/api/v1/accounts/:id/unpin` | user | write \| write:accounts | implemented (Phase 3) | In | alias `/unendorse` |
| POST | `/api/v1/accounts/:id/unendorse` | user | write \| write:accounts | implemented (Phase 3) | In | |
| GET | `/api/v1/accounts/:id/endorsements` | optional | read \| read:accounts | implemented (Phase 3) | In | empty array for unknown accounts |
| GET | `/api/v1/endorsements` | user | read \| read:accounts | implemented (Phase 3) | In | own endorsed accounts |
| POST | `/api/v1/apps` | none | — | implemented (Phase 2) | In | app registration; returns `client_secret` once |
| GET | `/api/v1/apps/verify_credentials` | app | — | planned (Phase 7) | In | |

### Follow requests

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/follow_requests` | user | follow \| read \| read:follows | implemented (Phase 3) | In | `REST::AccountSerializer[]` (requesters) |
| POST | `/api/v1/follow_requests/:id/authorize` | user | follow \| write \| write:follows | implemented (Phase 3) | In | 200 relationship of viewer → requester |
| POST | `/api/v1/follow_requests/:id/reject` | user | follow \| write \| write:follows | implemented (Phase 3) | In | |

### Domain blocks

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/domain_blocks` | user | follow \| read \| read:blocks | implemented (Phase 3; shape corrected 2026-10-17) | In | newest first; `max_id`/`since_id` are exclusive numeric row-id cursors; `limit` 100 (max 200); `Link` `next` on a full page and `prev` on a non-empty page; `422 This method requires an authenticated user` without a user token |
| POST | `/api/v1/domain_blocks` | user | follow \| write \| write:blocks | implemented (Phase 3; corrected 2026-10-17) | In | `domain` from the form body or query string; `200 {}`; blank → `422 Validation failed: Domain can't be blank`; malformed → `422 Validation failed: Domain is invalid`. Upstream's `AfterAccountDomainBlockWorker` (removing remote followers from the domain) arrives with federation (Phase 6) |
| DELETE | `/api/v1/domain_blocks` | user | follow \| write \| write:blocks | implemented (Phase 3; corrected 2026-10-17) | In | `domain` from the query string or form body; `200 {}`; idempotent |

### Statuses

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/statuses` | optional | read \| read:statuses | implemented (Phase 3) | In | multi-fetch `id[]` (≤20, else 422 `Mastodon::ValidationError`); invisible ids silently omitted; id-desc |
| GET | `/api/v1/statuses/:id` | optional | read \| read:statuses | implemented (Phase 3) | In | 404 `{"error":"Not Found"}` |
| POST | `/api/v1/statuses` | user | write \| write:statuses | implemented (Phase 3) | In | `quoted_status_id`/`quote_approval_policy`, `poll[...]`, `media_ids[]`, `Idempotency-Key` (1h replay) |
| PUT | `/api/v1/statuses/:id` | user | write \| write:statuses | implemented (Phase 3) | In | edit; no-op edit still 200 without bumping `edited_at` |
| PATCH | `/api/v1/statuses/:id` | user | write \| write:statuses | implemented (Phase 3) | In | alias of PUT |
| DELETE | `/api/v1/statuses/:id` | user | write \| write:statuses | implemented (Phase 3) | In | 200 pre-deletion body in source mode (`text`, no `content`) |
| GET | `/api/v1/statuses/:id/source` | user | read \| read:statuses | implemented (Phase 3) | In | 401 without token; `{id,text,spoiler_text}` |
| GET | `/api/v1/statuses/:id/context` | optional | read \| read:statuses | implemented (Phase 3) | In | 4096/4096 caps authenticated; 40/60 + depth 20 anonymous. Phase 6 adds `Mastodon-Async-Refresh` on authenticated reads (remote thread completion: descendants, and ancestors for logged-in viewers, deviation D5) |
| GET | `/api/v1/statuses/:id/history` | user | read \| read:statuses | implemented (Phase 3) | In | 401 without token; `REST::StatusEditSerializer[]` |
| PUT | `/api/v1/statuses/:id/interaction_policy` | user | write \| write:statuses | implemented (Phase 3) | In | `quote_approval_policy` only (`public\|followers\|nobody`) |
| PATCH | `/api/v1/statuses/:id/interaction_policy` | user | write \| write:statuses | implemented (Phase 3) | In | alias of PUT |
| POST | `/api/v1/statuses/:id/favourite` | user | write \| write:favourites | implemented (Phase 3) | In | idempotent; 200 status |
| POST | `/api/v1/statuses/:id/unfavourite` | user | write \| write:favourites | implemented (Phase 3) | In | 200 no-op when not favourited |
| POST | `/api/v1/statuses/:id/bookmark` | user | write \| write:bookmarks | implemented (Phase 3) | In | |
| POST | `/api/v1/statuses/:id/unbookmark` | user | write \| write:bookmarks | implemented (Phase 3) | In | 200 no-op |
| POST | `/api/v1/statuses/:id/reblog` | user | write \| write:statuses | implemented (Phase 3) | In | 200 NEW reblog record; idempotent |
| POST | `/api/v1/statuses/:id/unreblog` | user | write \| write:statuses | implemented (Phase 3) | In | 200 ORIGINAL; 200 no-op when not reblogged |
| POST | `/api/v1/statuses/:id/pin` | user | write \| write:accounts | implemented (Phase 3) | In | limit 5; four exact 422 messages |
| POST | `/api/v1/statuses/:id/unpin` | user | write \| write:accounts | implemented (Phase 3) | In | 200 no-op when not pinned |
| GET | `/api/v1/statuses/:id/favourited_by` | optional | read \| read:accounts | implemented (Phase 3) | In | accounts; cursor = favourite id (hidden in `Link`) |
| GET | `/api/v1/statuses/:id/reblogged_by` | optional | read \| read:accounts | implemented (Phase 3) | In | accounts; cursor = reblog id |
| GET | `/api/v1/statuses/:id/quotes` | user | read \| read:statuses | implemented (Phase 3) | In | 401 without token; accepted quotes only; cursor = quoting status id |
| POST | `/api/v1/statuses/:id/quotes/:quoting_status_id/revoke` | user | write \| write:statuses | implemented (Phase 3) | In | quoted author only; 200 = quoting status |
| POST | `/api/v1/statuses/:id/mute` | user | write \| write:mutes | planned (Phase 7) | In | conversation mute; ships with `conversations` (Phase 7) |
| POST | `/api/v1/statuses/:id/unmute` | user | write \| write:mutes | planned (Phase 7) | In | as above |
| POST | `/api/v1/statuses/:id/translate` | — | — | out of profile | Out | translation provider not configured |

### Polls

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/polls/:id` | optional | read \| read:statuses | implemented (Phase 3) | In | 404 when missing/invisible |
| POST | `/api/v1/polls/:id/votes` | user | write \| write:statuses | implemented (Phase 3) | In | `choices[]` required (else 400); single-choice re-vote 422 |

### Timelines, media, notifications, discovery (later phases)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/timelines/home` | user | read \| read:statuses | implemented (Phase 4) | In | own statuses + the most recent 1000 active follows, minus members of the viewer's exclusive lists (D10b, Phase 4.1); `limit` 20/max 40 (`limit=0`/non-numeric → empty, no `Link`); `local` accepted and ignored (as upstream v4.7.3); app-only token → 422; `Cache-Control: private, no-store` |
| GET | `/api/v1/timelines/public` | optional | read \| read:statuses | implemented (Phase 4) | In | public visibility only; self-replies kept, replies to others and reblogs dropped; `local`/`remote`/`only_media`; anonymous → `max-age=15, public, stale-while-revalidate=30, stale-if-error=86400`, authenticated → `private, no-store` |
| GET | `/api/v1/timelines/tag/{tag}` | optional | read \| read:statuses | implemented (Phase 4) | In | tag normalized (case-insensitive, leading `#` stripped); unknown or empty tag → `200 []` with no `Link`; public visibility only; replies kept; reblog wrappers not indexed (matches upstream, see deviations); `local`/`remote`/`only_media`; cache header as `public` above |
| GET | `/api/v1/timelines/list/{id}` | user | read \| read:lists | implemented (Phase 4) | In | members' statuses under the home gates; the list's `replies_policy` (`list`/`followed`/`none`) governs replies; not owned or unparsable id → 404 `{"error":"Record not found"}`; app-only token → 422; `Cache-Control: private, no-store` |
| GET | `/api/v1/timelines/link` | optional | read | planned (Phase 7) | In | |
| GET | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | 206 while unprocessed; 422 on processing failure (Mastodon's controller authorizes write on every action, `show` included) |
| POST | `/api/v1/media` | user | write \| write:media | implemented (Phase 4) | In | media v1 (multipart, inline processing → 200) |
| PUT | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | description/focus/thumbnail; 206 while unprocessed |
| DELETE | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | 422 while attached to a status |
| POST | `/api/v2/media` | user | write \| write:media | implemented (Phase 4) | In | media v2 (multipart; video/audio delayed → 202, images inline → 200) |
| GET | `/api/v1/notifications` | user | read:notifications | implemented (Phase 4) | In | `limit` 20/max 40 with `limit_param` semantics (`limit=0` → empty page); `types[]`/`exclude_types[]` silently drop unknown values; `group_key` uses the `ungrouped-<id>` fallback (no server-side merging in the v1 profile) |
| GET | `/api/v1/notifications/:id` | user | read:notifications | implemented (Phase 4) | In | 404 `{"error":"Record not found"}` for foreign/missing |
| POST | `/api/v1/notifications/clear` | user | write \| write:notifications | implemented (Phase 4) | In | `200 {}` |
| GET | `/api/v1/notifications/unread_count` | user | read:notifications | implemented (Phase 4) | In | `{"count": N}` = rows newer than the notifications marker (limit default 100, max 1000) |
| POST | `/api/v1/notifications/:id/dismiss` | user | write \| write:notifications | implemented (Phase 4) | In | `200 {}`; repeat → 404 |
| GET | `/api/v1/notifications/requests` | user | read:notifications | implemented (Phase 4) | In | always `[]`: this server has no notification-policy filter pipeline yet, so no requests can exist (never fabricated) |
| GET | `/api/v1/notifications/requests/merged` | user | read:notifications | implemented (Phase 4) | In | `{"merged": true}` (no unmerge work exists) |
| GET/POST | `/api/v1/markers` | user | read:statuses / write:statuses | implemented (Phase 4) | In | `timeline[]` on GET; `home[last_read_id]`/`notifications[last_read_id]` on POST (query or body); response `{timeline:{last_read_id,version,updated_at}}`; fresh marker reports `version: 1` |
| GET | `/api/v1/preferences` | user | read \| read:accounts | implemented (Phase 2) | In | incl. quote-policy defaults |
| GET | `/api/v1/filters` | user | read \| read:filters | implemented (Phase 4) | In | v1 projection over keyword rows (deprecated upstream since 2022-11-14, but pinned v4.7.3 still serves it); each row's `id` is the keyword id |
| POST | `/api/v1/filters` | user | write \| write:filters | implemented (Phase 4) | In | creates filter + one keyword in ONE transaction |
| GET | `/api/v1/filters/:id` | user | read \| read:filters | implemented (Phase 4) | In | |
| PUT | `/api/v1/filters/:id` | user | write \| write:filters | implemented (Phase 4) | In | multi-keyword parent-attribute guard answers the exact deprecation 422 |
| DELETE | `/api/v1/filters/:id` | user | write \| write:filters | implemented (Phase 4) | In | deletes the keyword row only (upstream v1 semantics) |
| GET | `/api/v1/lists` | user | read | implemented (Phase 4) | In | all of the viewer's lists (v4.7.3 does not paginate this route) |
| POST | `/api/v1/lists` | user | write | implemented (Phase 4) | In | title blank/long and the 50-list limit answer the exact 422 bodies; `replies_policy` and `exclusive` accepted at creation (Phase 4.1) |
| GET | `/api/v1/lists/:id` | user | read | implemented (Phase 4) | In | foreign/missing → 404 `{"error":"Record not found"}` |
| PUT | `/api/v1/lists/:id` | user | write | implemented (Phase 4) | In | also PATCH; `replies_policy` validated against the enum |
| DELETE | `/api/v1/lists/:id` | user | write | implemented (Phase 4) | In | `200 {}` |
| GET | `/api/v1/lists/:id/accounts` | user | read \| read:lists | implemented (Phase 4) | In | `limit` default 40 (`limit=0` → all), `Link` cursors |
| POST | `/api/v1/lists/:id/accounts` | user | write \| write:lists | implemented (Phase 4) | In | `account_ids[]`; membership requires the owner's active follow/pending request (422 `Account must be following`), unknown id → 404 |
| DELETE | `/api/v1/lists/:id/accounts` | user | write \| write:lists | implemented (Phase 4) | In | idempotent `200 {}` |
| GET | `/api/v2/filters` | user | read \| read:filters | implemented (Phase 4) | In | v2 model with `keywords[]` + `statuses[]` |
| POST | `/api/v2/filters` | user | write \| write:filters | implemented (Phase 4) | In | `title`/`context[]`/`expires_in`/`filter_action`/`keywords_attributes` |
| GET | `/api/v2/filters/:id` | user | read \| read:filters | implemented (Phase 4) | In | |
| PATCH | `/api/v2/filters/:id` | user | write \| write:filters | implemented (Phase 4) | In | indexed `keywords_attributes[N][...]` supported |
| DELETE | `/api/v2/filters/:id` | user | write \| write:filters | implemented (Phase 4) | In | `200 {}`-equivalent empty payload semantics |
| GET/POST | `/api/v2/filters/:filter_id/keywords` | user | read/write:filters | implemented (Phase 4) | In | `keyword`, `whole_word` |
| GET/PATCH/DELETE | `/api/v2/filters/keywords/:id` | user | read/write:filters | implemented (Phase 4) | In | |
| GET/POST | `/api/v2/filters/:filter_id/statuses` | user | read/write:filters | implemented (Phase 4) | In | `status_id`; not-fetchable → 404 |
| GET/DELETE | `/api/v2/filters/statuses/:id` | user | read/write:filters | implemented (Phase 4) | In | |
| GET | `/api/v1/scheduled_statuses` | user | read | implemented (Phase 4) | In | id-desc, `Link` cursors (20/40) |
| GET | `/api/v1/scheduled_statuses/:id` | user | read | implemented (Phase 4) | In | foreign/missing → 404 `{"error":"Record not found"}` |
| PUT | `/api/v1/scheduled_statuses/:id` | user | write | implemented (Phase 4) | In | also PATCH; `scheduled_at` only; blank → 422 |
| DELETE | `/api/v1/scheduled_statuses/:id` | user | write | implemented (Phase 4) | In | `200 {}`; the publish job becomes a no-op |
| GET | `/api/v1/search` | — | — | out of profile (route absent in v4.7.3) | Out | removed upstream in Mastodon 3.0 (commit `18331fefa2`); only v2 exists in the pinned tag |
| GET | `/api/v2/search` | optional | read:search | implemented (Phase 4) | In | local corpus only: accounts by handle/display name, hashtags by prefix, statuses by bounded recent scan (authenticated only, like `status_searchable?`); `resolve` without auth → pinned 401; `collections` always `[]`. Interim until the Phase 7.2 engine (see Search deviations) |
| GET | `/api/v1/streaming` | user | read | planned (Phase 5) | In | SSE/WebSocket |
| GET | `/api/v1/streaming/(*any)` | user | read | planned (Phase 5) | In | |

### Conversations (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/conversations` | user | read \| read:statuses | planned (Phase 7) | In | per-viewer conversation list |
| DELETE | `/api/v1/conversations/:id` | user | write \| write:conversations | planned (Phase 7) | In | per-viewer state |
| POST | `/api/v1/conversations/:id/read` | user | write \| write:conversations | planned (Phase 7) | In | |
| POST | `/api/v1/conversations/:id/unread` | user | write \| write:conversations | planned (Phase 7) | In | |

### Suggestions (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/suggestions` | user | read \| read:accounts | planned (Phase 7) | In | deprecated upstream (`Deprecation` and `Sunset` headers from 2021-05-16); `limit` and `offset` |
| DELETE | `/api/v1/suggestions/:id` | user | write \| write:accounts | planned (Phase 7) | In | `:id` is the suggested account id; `render_empty` |
| GET | `/api/v2/suggestions` | user | read \| read:accounts | planned (Phase 7) | In | `REST::SuggestionSerializer[]`; `Mastodon-Async-Refresh` when a FASP retrieval runs (feature-flagged upstream). FASP-sourced entries are out of profile until a FASP integration exists; never fabricated |
| DELETE | `/api/v2/suggestions/:id` | user | write \| write:accounts | planned (Phase 7) | In | `:id` is the suggested account id |

### Announcements (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/announcements` | user (any scope) | — | planned (Phase 7) | In | `require_user!` only (no scope check); published announcements, chronological; authored by CLI |
| POST | `/api/v1/announcements/:id/dismiss` | user | write \| write:accounts | planned (Phase 7) | In | `render_empty` |
| PUT | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | planned (Phase 7) | In | `:id` is the reaction name; `render_empty` |
| PATCH | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | planned (Phase 7) | In | alias of PUT |
| DELETE | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | planned (Phase 7) | In | removes the viewer's reaction by name |

### Donation campaigns (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/donation_campaigns` | user (any scope) | — | planned (Phase 7) | In | `204` when no campaign API URL is configured, or when no campaign is returned. Otherwise an outbound GET to the operator-configured URL through the safe client, cached 1 h, circuit-broken after repeated failures |

### Web Push (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/push/subscription` | user | push | planned (Phase 7) | In | subscription bound to the access token; VAPID keys from configuration |
| GET | `/api/v1/push/subscription` | user | push | planned (Phase 7) | In | |
| PUT | `/api/v1/push/subscription` | user | push | planned (Phase 7) | In | update; `check_push_subscription` guard |
| PATCH | `/api/v1/push/subscription` | user | push | planned (Phase 7) | In | alias of PUT |
| DELETE | `/api/v1/push/subscription` | user | push | planned (Phase 7) | In | |

### `/api/web/*` (Phase 7)

These routes serve keystodon's first-party mode. Upstream guards them with the Devise session and CSRF (`protect_from_forgery with: :exception`), not OAuth bearer tokens. birchtree's plan does the same (Phase 7 prerequisites in `birchtree.md` §7). Third-party clients never use them. Under the proposed default, first-party `/api/v1/*` calls use OAuth tokens (`docs/compatibility-profile.md`).

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| PUT | `/api/web/settings` | session + CSRF | — | planned (Phase 7) | In | web UI settings sync; `require_user!` |
| PATCH | `/api/web/settings` | session + CSRF | — | planned (Phase 7) | In | alias of PUT |
| GET | `/api/web/embeds/:id` | none | — | planned (Phase 7) | In | local status: OEmbed JSON, `show?` authorization. Remote status: requires a signed-in session and performs an outbound oEmbed fetch through the safe client |
| POST | `/api/web/push_subscriptions` | session + CSRF | — | planned (Phase 7) | In | `require_user!` |
| PUT | `/api/web/push_subscriptions/:id` | session + CSRF | — | planned (Phase 7) | In | `require_user!` |
| PATCH | `/api/web/push_subscriptions/:id` | session + CSRF | — | planned (Phase 7) | In | alias of PUT |
| DELETE | `/api/web/push_subscriptions/:id` | session + CSRF | — | planned (Phase 7) | In | no `require_user!` guard upstream (destroy only) |

### Trends, public (Phase 7.3)

Upstream serves only admin-approved items (`Trends.*.query.allowed`) and returns nothing when trends are disabled (`Setting.trends`). Pagination is `offset` and `limit` (default 10), not cursors.

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/trends` | optional | — | planned (Phase 7.3) | In | deprecated alias of `/api/v1/trends/tags` (`Deprecation` from 2022-03-30); `cache_if_unauthenticated!` |
| GET | `/api/v1/trends/tags` | optional | — | planned (Phase 7.3) | In | approved tags, locale-filtered; authenticated viewers get `filtered_for` |
| GET | `/api/v1/trends/links` | optional | — | planned (Phase 7.3) | In | approved preview cards only; needs link-preview crawling (safe client) |
| GET | `/api/v1/trends/statuses` | optional | — | planned (Phase 7.3) | In | approved statuses only |

### Reports (Phase 7.3)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/reports` | user | write \| write:reports | planned (Phase 7.3) | In | creates a report (`require_user!`); remote forwarding as Flag is Phase 6 |

### Annual reports (Phase 7.3)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/annual_reports` | user | read \| read:accounts | planned (Phase 7.3) | In | index of the viewer's annual reports |
| GET | `/api/v1/annual_reports/:id` | user | read \| read:accounts | planned (Phase 7.3) | In | |
| POST | `/api/v1/annual_reports/:id/read` | user | write \| write:accounts | planned (Phase 7.3) | In | marks the report read |
| POST | `/api/v1/annual_reports/:id/generate` | user | write \| write:accounts | planned (Phase 7.3) | In | enqueues generation as a durable job |
| GET | `/api/v1/annual_reports/:id/state` | user | read \| read:accounts | planned (Phase 7.3) | In | generation state |

### Admin API (Phase 7.3)

All admin rows: auth `user + role`. The role is `admin` (every admin route) or `moderator` (reports and account routes; the split is proposed in `birchtree.md` §7 Phase 7.3). Reads take `admin:read` (or the listed sub-scope). Writes take `admin:write` (or the listed sub-scope). Each row is gated by the moderator-or-admin role check before its scope check. Upstream sources: `app/controllers/api/v1/admin/*` and `app/controllers/api/v2/admin/accounts_controller.rb` at v4.7.3.

#### Accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/accounts` | user + role | admin:read \| admin:read:accounts | planned (Phase 7.3) | In | `REST::Admin::AccountSerializer[]` |
| GET | `/api/v1/admin/accounts/:id` | user + role | admin:read \| admin:read:accounts | planned (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/accounts/:id` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | enqueues account deletion (upstream: async worker) |
| POST | `/api/v1/admin/accounts/:id/enable` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | local accounts only (`require_local_account!`) |
| POST | `/api/v1/admin/accounts/:id/unsensitive` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/accounts/:id/unsilence` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/accounts/:id/unsuspend` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | enqueues unsuspension (upstream: async worker) |
| POST | `/api/v1/admin/accounts/:id/approve` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | local accounts only; depends on approval registration modes (Phase 2 gap) |
| POST | `/api/v1/admin/accounts/:id/reject` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | local accounts only; same dependency as `approve` |
| POST | `/api/v1/admin/accounts/:account_id/action` | user + role | admin:write \| admin:write:accounts | planned (Phase 7.3) | In | account action (`Admin::AccountAction`); the nested path parameter is `:account_id` |

#### Reports (moderator queue)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/reports` | user + role | admin:read \| admin:read:reports | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/reports/:id` | user + role | admin:read \| admin:read:reports | planned (Phase 7.3) | In | |
| PUT | `/api/v1/admin/reports/:id` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/reports/:id` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | alias of PUT |
| POST | `/api/v1/admin/reports/:id/assign_to_self` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/unassign` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/reopen` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/resolve` | user + role | admin:write \| admin:write:reports | planned (Phase 7.3) | In | |

#### Domain allows, domain blocks, email-domain blocks, IP blocks

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/domain_allows` | user + role | admin:read \| admin:read:domain_allows | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/domain_allows/:id` | user + role | admin:read \| admin:read:domain_allows | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/domain_allows` | user + role | admin:write \| admin:write:domain_allows | planned (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/domain_allows/:id` | user + role | admin:write \| admin:write:domain_allows | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/domain_blocks` | user + role | admin:read \| admin:read:domain_blocks | planned (Phase 7.3) | In | server-level blocks; enforced at federation ingress and egress (Phase 6) |
| GET | `/api/v1/admin/domain_blocks/:id` | user + role | admin:read \| admin:read:domain_blocks | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/domain_blocks` | user + role | admin:write \| admin:write:domain_blocks | planned (Phase 7.3) | In | |
| PUT | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | planned (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | planned (Phase 7.3) | In | alias of PUT |
| DELETE | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/email_domain_blocks` | user + role | admin:read \| admin:read:email_domain_blocks | planned (Phase 7.3) | In | registration-time enforcement needs email confirmation (Phase 2 gap) |
| GET | `/api/v1/admin/email_domain_blocks/:id` | user + role | admin:read \| admin:read:email_domain_blocks | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/email_domain_blocks` | user + role | admin:write \| admin:write:email_domain_blocks | planned (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/email_domain_blocks/:id` | user + role | admin:write \| admin:write:email_domain_blocks | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/ip_blocks` | user + role | admin:read \| admin:read:ip_blocks | planned (Phase 7.3) | In | enforcement needs a request-path check (trusted-proxy client IP, Phase 1) |
| GET | `/api/v1/admin/ip_blocks/:id` | user + role | admin:read \| admin:read:ip_blocks | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/ip_blocks` | user + role | admin:write \| admin:write:ip_blocks | planned (Phase 7.3) | In | |
| PUT | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | planned (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | planned (Phase 7.3) | In | alias of PUT |
| DELETE | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | planned (Phase 7.3) | In | |

#### Trends approval

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/trends/tags` | user + role | admin:read | planned (Phase 7.3) | In | no sub-scope upstream |
| POST | `/api/v1/admin/trends/tags/:id/approve` | user + role | admin:write | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/tags/:id/reject` | user + role | admin:write | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/links` | user + role | admin:read | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/:id/approve` | user + role | admin:write | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/:id/reject` | user + role | admin:write | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/links/publishers` | user + role | admin:read | planned (Phase 7.3) | In | preview card providers (`path: :publishers` upstream) |
| POST | `/api/v1/admin/trends/links/publishers/:id/approve` | user + role | admin:write | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/publishers/:id/reject` | user + role | admin:write | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/statuses` | user + role | admin:read | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/statuses/:id/approve` | user + role | admin:write | planned (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/statuses/:id/reject` | user + role | admin:write | planned (Phase 7.3) | In | |

#### Metrics

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/admin/measures` | user + role | admin:read | planned (Phase 7.3) | In | POST with a read scope upstream (kept as upstream) |
| POST | `/api/v1/admin/dimensions` | user + role | admin:read | planned (Phase 7.3) | In | POST with a read scope upstream |
| POST | `/api/v1/admin/retention` | user + role | admin:read | planned (Phase 7.3) | In | POST with a read scope upstream |

#### Canonical email blocks and tags

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/canonical_email_blocks` | user + role | admin:read \| admin:read:canonical_email_blocks | planned (Phase 7.3) | In | registration-time enforcement needs email confirmation (Phase 2 gap) |
| POST | `/api/v1/admin/canonical_email_blocks/test` | user + role | admin:read \| admin:read:canonical_email_blocks | planned (Phase 7.3) | In | read scope upstream (`test` is in the read guard) |
| POST | `/api/v1/admin/canonical_email_blocks` | user + role | admin:write \| admin:write:canonical_email_blocks | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/canonical_email_blocks/:id` | user + role | admin:read \| admin:read:canonical_email_blocks | planned (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/canonical_email_blocks/:id` | user + role | admin:write \| admin:write:canonical_email_blocks | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/tags` | user + role | admin:read | planned (Phase 7.3) | In | |
| GET | `/api/v1/admin/tags/:id` | user + role | admin:read | planned (Phase 7.3) | In | |
| PUT | `/api/v1/admin/tags/:id` | user + role | admin:write | planned (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/tags/:id` | user + role | admin:write | planned (Phase 7.3) | In | alias of PUT |

#### v2 admin accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v2/admin/accounts` | user + role | admin:read \| admin:read:accounts | planned (Phase 7.3) | In | v1 admin accounts index with filter parameters (`origin`, `status`, `permissions`, `username`, `by_domain`, `display_name`, `email`, `ip`, `invited_by`, `role_ids`); `Link` cursors (`max_id`, `min_id`) |

### Bullet-only entries (excluded, or not yet rowed)

Removed from this section in the 2026-10-17 re-scope: `annual_reports`, `announcements`, `conversations`, `donation_campaigns`, `suggestions`, `POST /api/v1/reports`, `namespace :admin`, `v2` admin, `push/*`, `web/*`. Each is now a row above with a planned phase. The entries below remain as bullets because they are not yet rowed or are excluded.

- `GET /api/v1/oembed` — public oEmbed; not in initial profile (record as gap if clients need it). **Out.**
- `namespace :v1_alpha` — experimental/legacy (`accounts/collections`, `collections/*`); explicitly **Out**.
- `GET /api/v1/custom_emojis` — **implemented (Phase 4)**: public index over local + enabled + picker-visible emoji (CLI-managed; no admin API in this milestone). Emoji embedding in status payloads is implemented; account-payload emoji remain `[]` (Phase 7). Listed here for historical reasons; it has no row yet (see discrepancies).
- `GET /api/v1/blocks`, `GET /api/v1/mutes`, `GET /api/v1/favourites`, `GET /api/v1/bookmarks` — relation collection reads; **planned (Phase 7)**. No rows yet (see discrepancies).
- `accounts/familiar_followers`, `identity_proofs`, `featured_tags`, `email_subscriptions`, `collections`, `tags` follow/feature, `followed_tags` — **planned (Phase 7)**. No rows yet (see discrepancies).
- `translate` (statuses) — **Out** unless a translation provider is configured (row above).

## Non-route contract items tracked separately

- IDs: decimal strings on the wire, numeric ordering internally (§4 of `birchtree.md`).
- Pagination: `Link` `next`/`prev` headers; cursors numeric `Id`s (status, account, interaction and domain-block row ids). Cursor values are opaque to clients; they are swapped in place and other query params are preserved. Trends use `offset`/`limit` instead (Phase 7.3).
- Error shapes: `{"error": "..."}`; statuses 200/400/401/403/404/405?/422/429 where applicable. Two 404 bodies exist and are reproduced: `Not Found` (explicit `not_found`, status sub-routes) and `Record not found` (unrescued lookup, update/delete).
- Rate-limit headers, streaming event envelopes (Phase 5).
- Deprecation headers: `Deprecation` (`@<unix timestamp>`) and, where upstream sets it, `Sunset`. Applies to `GET /api/v1/suggestions` and `GET /api/v1/trends` (Phase 7 / 7.3).
- Async-refresh header: `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]` (Phase 6 remote thread completion; Phase 7 suggestions FASP path).
- Idempotency: `Idempotency-Key` header on `POST /api/v1/statuses` only (1-hour retention, per-account scoping — matches Mastodon's `SETEX 3600` namespace). Other write endpoints are naturally idempotent (unique rows) instead.
- Admin scope families: `admin:read` and `admin:write`, with per-resource sub-scopes (`admin:read:accounts`, `admin:write:reports`, and so on), as registered by the pinned Doorkeeper configuration. Not yet in birchtree's scope registry (Phase 7.3).

## Known Phase 3 deviations / gaps (honesty list)

- **JSON request bodies: resolved in Phase 4.1 (D11, 2026-10-17).** Every REST write endpoint that reads a body accepts `application/json` as well as form and empty bodies. See "Phase 4.1: JSON bodies and exclusive lists" below. The Phase 3 wording, "not accepted", no longer applies.
- **Tag objects** in status payloads use the tag name as `id` (no tag table yet); `history`/`following`/`featuring` are contract-shaped defaults. Real tag rows land with the Phase 7 tag surface.
- **Conversation mutes** (`POST /api/v1/statuses/:id/mute|unmute`) are planned with `conversations` (Phase 7). Until then, the status DTO's `muted` flag is emitted with its neutral `false` for authenticated viewers.
- **`application` on statuses** is shown whenever present (Mastodon's `user_shows_application?` preference defaults to true and is not stored yet).
- Media attachment DTOs render `url`/`preview_url` as `null` until the attachment is processed (Phase 4 upload + processing lifecycle are live) — contract-correct "not processed" state for queued/in-progress/failed rows.
- Media file serving (`GET /system/media_attachments/files/{*path}`) serves the blob bytes with the sniffed content type and `Cache-Control: max-age=86400, public`.

## Known Phase 4 media deviations / gaps (honesty list)

- **No transcoding toolchain (no ffmpeg), interim until Phase 7.1**: animated GIF uploads keep the wire type `image`. Mastodon produces `gifv` with an MP4 rendition (`lib/paperclip/gif_transcoder.rb`). **Deliberate deviations, decided 2026-10-17 (Phase 7.1):** animated GIF becomes WebM with VP9, typed `video`; video becomes WebM with VP9; still images become WebP (D4); a video already in VP9 WebM within the upload size limit passes through unchanged; the server never strips or rewrites metadata. `gifv` is never produced. Clients that give `gifv` autoplay and loop treatment will show these as ordinary videos. Until Phase 7.1 ships, video/audio keep their original bytes untouched; `meta.small`/`preview_url` exist only when an image preview was generated; duration/frame rate/bitrate stay `null` (MP4 `tkhd` dimensions are read when present).
- **Blob keys are sharded by a per-upload random 16-hex name** (`{aa}/{bb}/{random16}/original.{ext}`), not the attachment id: bytes must be staged *before* the id-minting commit (`birchtree.md` §5), so the id cannot appear in its own key. The wire URLs are unaffected.
- **The upload size cap equals the multipart body limit** (`ApiState` carries no `Config`); `Config::validate` enforces `max_file_size ≤ max_body_size`, so the effective cap matches the default 16 MB. The 422 message formats the cap like Rails `number_to_human_size` ("16 MB", "1 KB", …).
- **A non-numeric `focus` param answers `422 Validation failed: Focus is invalid`** instead of Ruby's `Float()` coercion to `0.0`; a blank focus is ignored (upload) or clears the point (update).
- **An image upload carrying a thumbnail answers "Thumbnail must be blank"** before the thumbnail's own content-type/size checks (Mastodon's model validates the thumbnail's content type first); the pinned messages are otherwise exact.

## Search deviations (interim until Phase 7.2)

- **Local corpus only.** `GET /api/v2/search` searches the local database: accounts by handle and display name, hashtags by prefix, and statuses by a bounded recent scan.
- **Upstream returns no status hits without a search engine.** Mastodon's `SearchService#status_searchable?` requires `Chewy.enabled?` (Elasticsearch). Birchtree's status results therefore exist where upstream's would be empty. This is interim, not parity. Phase 7.2 keeps this scan as the `database` mode and adds an `embedded` default (an in-process full-text library) and an `off` mode that matches upstream with no engine.
- **`resolve` (remote lookup)** depends on federation (Phase 6). Unauthenticated `resolve` returns the pinned 401.
- **`collections`** is always `[]`.
- **Default mode (D6, accepted 2026-10-17):** search is on by default (`embedded`), so status hits appear that Mastodon's default install (Elasticsearch disabled) does not show. An administrator can switch it off (`off`).

## Phase 4.1: JSON bodies and exclusive lists (honesty list)

Implemented 2026-10-17 (D10a, D10b, D11). Evidence: `tests/phase4_1_json_parity.rs`, `tests/phase4_1_exclusive_home.rs`, `tests/phase4_1_home_cap.rs`, `docs/logs/2026-10-17-phase-4-1-verification.md`.

- **Which requests take JSON.** `Content-Type: application/json` (with or without `charset`) is parsed on every route that reads a body: the form-taking POST/PUT/PATCH/DELETE routes in `router.rs`, including `POST /oauth/token` and `POST /oauth/revoke`. Body-less write routes accept a JSON body and ignore it (after the malformed-JSON check below). Form and query parameters are unchanged.
- **Flattening** (`src/api/json_body.rs`, following Rails' default parameter parsing, which upstream does not override): nested objects become `a[b]`; arrays of scalars repeat `a[]`; arrays of objects use indexed keys `a[N][field]`, the form encoding that `keywords_attributes[N][...]` already uses; numbers become decimal strings; booleans become `"true"`/`"false"`; `null` is absent. A top-level non-object body is wrapped under `_json`, as Rails does.
- **Excluded: `POST /oauth/authorize`.** It is the browser consent form, which upstream guards with a CSRF token (`ErrorResponses`, `protect_from_forgery`), so a JSON client cannot submit it. It stays form-only: a JSON body there answers `415`, as before. Multipart media uploads (`POST /api/v1/media`, `POST /api/v2/media`) are unchanged.
- **Other content types** keep their existing behaviour: `LenientForm` routes parse the body as a form; the `FormBody` routes (accounts register and update, apps, `/oauth/token`, `/oauth/revoke`) answer `415` as before.
- **Malformed JSON** is refused before any handler runs, for every route (Rails parses params before the action, so it fails even on a body-less route and before authentication). Upstream's answer, derived from Rails 8.1.3.1 (`ActionDispatch::Http::Parameters` raises `ParseError`; `ShowExceptions` hands it to `PublicExceptions`; the v4.7.3 tree has no `public/400.html`):
  - when the first negotiated format is JSON (`Accept`, q-sorted; `X-Requested-With` and the content type count as Rails does): `400`, `Content-Type: application/json; charset=utf-8`, body `{"status":400,"error":"Bad Request"}`;
  - otherwise: `400`, `Content-Type: text/html; charset=utf-8`, empty body.
  - Not replicated: the `Vary: Authorization` header that birchtree sends on every `/api/` response (upstream's exceptions-app response does not carry the controller's headers). Clients do not read it.
- **Residual differences.** A malformed JSON body sent to an unknown `/api/` path answers `400`, where upstream answers `404`: the guard runs before routing. A JSON body over 1 MiB answers `413` (Rails has no JSON cap of its own; the web server's limit applies upstream).
- **Source-derived, not run against upstream.** Upstream's JSON answers come from the pinned source (see above), not from a live v4.7.3 instance. No Ruby or Postgres is installed on the development machine. `birchtree.md` §8 item 11 is still the parity harness.
- **Exclusive lists on home (D10b).** Implemented as `FeedManager#filter_from_home` does. `exclusive=true` lists only; `list` timelines unaffected; the viewer's own statuses (including the viewer's reblogs) never left out. The membership check is one read transaction per home request, with cost = the viewer's exclusive lists times the home authors (the list cap is 50 per account).
- **List creation honours `replies_policy` and `exclusive`** (Phase 4.1). Upstream's `ListsController#create` takes them through `list_params`; birchtree's create previously ignored both, so a client creating an exclusive list got a non-exclusive one. An invalid `replies_policy` answers `422` (the update path's message); upstream's answer for an invalid enum is not verified here.
- **Home cap 1000 (D10a).** The home reads the 1000 most recent follows. The store's scan changed from a per-author window and sort to a lazy k-way merge (one cursor per author and a heap of cursor heads; `birchtree.md` §5). The old merge let the oldest follows drop out below the cap: with 1000 follows and six posts each, a fresh post from the oldest follow vanished from home entirely (three posts each did not trigger it; measured, see the Phase 4.1 verification log). The merge now reads only what the page needs.

## Known Phase 4 timelines deviations (honesty list)

- **No `206 Mastodon-Async-Refresh` regeneration.** Timelines are fan-out-on-read (`birchtree.md` §2.6): nothing is materialized, so there is no feed to regenerate and the upstream 206 path never applies. Architectural difference, not a gap. Accepted 2026-10-17 (D9): the home feed is served complete and never returns `206`.
- **`Link` echoes the whole query minus the cursors** (`max_id`, `since_id`, `min_id`). Upstream echoes only its permitted parameters. Clients that follow the `Link` URL get the same page either way.
- **Authenticated public and tag responses are `private, no-store`**, not the public `max-age=15` header. They carry viewer-specific data (`filtered`, favourited/reblogged flags), so a shared cache must not store them. Anonymous responses keep the documented public header. The plan's literal table said `public` for every public/tag response; this is a deliberate narrowing.
- **Home reads the 1000 most recent active follows** (`HOME_AUTHOR_CAP`, raised from 500 in Phase 4.1). Older follows are not shown. Bounded scan; not parity with an unbounded fan-out (D10a, accepted 2026-10-17).
- **Exclusive lists are hidden from home (D10b, implemented in Phase 4.1).** Members of the viewer's `exclusive=true` lists are left out of home, as `FeedManager#filter_from_home` does (`:skip_home`). The viewer's own statuses are never left out, and list timelines are unaffected. Evidence: `tests/phase4_1_exclusive_home.rs`.
- **Own statuses always show in the viewer's own home feed**, including direct statuses and replies to strangers. The policy exempts the viewer's own statuses from the home gates.
- **Tag feed excludes reblog wrappers, matching upstream (closed 2026-10-17).** Wrappers carry no hashtags of their own, and upstream's `ReblogService` creates reblog rows with no tag rows, so upstream's tag feed also excludes them. Pinned by `tag_timeline_normalizes_case_and_keeps_replies`.
- **Public feed keeps self-replies by policy, not by the store**: the store scans replies and the filter keeps only self-replies and originals.
- **Per-request account facts are capped at 2000 distinct accounts** (`ACCOUNT_CACHE_CAP`; was 500). The home follows fill the first 1000 entries, and reblog targets and reply parents use the rest, so a viewer at the home cap still sees reblogs of strangers. Past the cap, an unresolved account fails closed: its statuses are excluded, never shown. The store's author cap is 1001 (the viewer plus 1000 follows; was 500, which silently dropped the oldest follows).
- **Remote accounts do not exist yet** (federation is Phase 6). `remote=true` is therefore only exercised as empty, and the domain-block rule is exercised through the local domain.
- **`replies_policy=none` on lists** keeps only self-replies and replies to the viewer; `followed` keeps replies to followed parents; `list` keeps replies to members. Replies to the viewer pass under every policy.

## Route inventory discrepancies and gaps (found 2026-10-17)

Recorded, not fixed. Each needs a decision or a contract-review pass.

1. **Domain-block route shape: resolved 2026-10-17.** The Phase 3 rows now match the pinned tag: `GET`, `POST` and `DELETE /api/v1/domain_blocks`, with numeric row-id paging and the domain as a parameter. The correction is recorded in `birchtree.md` §7 Phase 3 item 6 and in `docs/logs/2026-10-17-domain-blocks-correction.md`.
2. **Pinned routes not yet rowed, outside this re-scope.** Each needs a row in Phase 7 or later:
   - `profile` (`GET`/`PATCH /api/v1/profile`, `DELETE` of avatar and header);
   - `directory` (`GET /api/v1/directory`);
   - `instance` sub-resources (`peers`, `rules`, `domain_blocks`, `terms_of_service` with `:date`, `privacy_policy`, `extended_description`, `translation_languages`, `languages`, `activity`), and `peers/search`;
   - `emails/confirmations` (`POST`) and `emails/check_confirmation`;
   - `domain_blocks/preview`;
   - `tags` (`GET /api/v1/tags/:id`; `follow`, `unfollow`, `feature`, `unfeature`) and `followed_tags`; `featured_tags` (top-level and per-account);
   - accounts sub-resources: `lists`, `identity_proofs`, `familiar_followers`, `email_subscriptions`, `collections`, `in_collections`; top-level `collections` (`GET`/`POST`/`PUT`/`DELETE`, and `items`/`revoke`);
   - notifications: `requests/:id` (show), `requests/:id/accept|dismiss`, the bulk `requests/accept|dismiss`, and `notifications/policy` (`GET`/`PUT`);
   - `GET /api/v2/notifications` and its `clear`, `unread_count`, `dismiss`, `:group_key/accounts`, and `policy` sub-routes.
3. **`GET /api/v1/accounts` (index) had no row.** Resolved in the 2026-10-17 follow-up: the row is in profile at Phase 7, as the account multi-fetch (`id[]`, up to 40).
4. **`namespace :web` and `/api/web/*` auth.** Resolved as a design decision in the 2026-10-17 follow-up: these routes serve keystodon's first-party mode and use the browser session plus CSRF (Phase 7). Still open: whether first-party `/api/v1/*` calls use OAuth tokens (proposed) or the session.
5. **Scope registry comment.** `src/api/oauth/scopes.rs` says the admin scopes "are accepted for storage but never granted". `KNOWN_SCOPES` does not list any `admin:*` scope, so they are rejected today. This is a comment or code mismatch for Phase 7.3 to resolve.
6. **`/api/v1/trends` and `/api/v1/trends/tags` in the pinned tag** serve the same action (`trends/tags#index`). Birchtree's rows keep both; the alias is deprecated upstream.
