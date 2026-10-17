# docs/endpoint-manifest.md

Status: Draft

Route inventory from Mastodon v4.7.3 `config/routes/api.rb` (commit `8ff11c2b`), cross-checked against the public API docs where practical. One manifest row per method/path. Profile column: `In` = in the declared Phase 0/1+ compatibility profile, `Out` = explicitly excluded (admin/moderation, legacy, experimental, or not yet scoped), `Plan` = planned for a later phase. Status column: `implemented (Phase N)` = route exists with contract tests; `planned (Phase N)` = in profile but intentionally absent (404) until that phase — never a success-shaped stub.

Auth scopes are Mastodon conventions from the source/docs. They are enforced centrally (Phase 2) at the route boundary: a missing/invalid token yields `401 {"error":"The access token is invalid"}`, an insufficient scope yields `403 {"error":"This action is outside the authorized scopes"}`, and a user endpoint reached with an app-only token yields `422 {"error":"This method requires an authenticated user"}` (or `403 {"error":"This method requires an client credentials authentication"}` for the reverse case on registration). Scope lists are "any-of" (broad OR granular), matching Doorkeeper's `any?`.

## OAuth / discovery (public, not in config/routes/api.rb)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/.well-known/webfinger` | none | — | planned (Phase 6) | Plan | WebFinger `acct:` resolution |
| GET | `/.well-known/nodeinfo` | none | — | planned (Phase 6) | Plan | NodeInfo discovery |
| GET | `/nodeinfo/2.0` | none | — | planned (Phase 6) | Plan | NodeInfo 2.0 document |
| GET/POST | `/oauth/authorize` | session | — | implemented (Phase 2) | In | Doorkeeper authorize; login + consent rendered server-side |
| POST | `/oauth/token` | basic/client | — | implemented (Phase 2) | In | `authorization_code` (PKCE S256) + `client_credentials` |
| POST | `/oauth/revoke` | basic/client | — | implemented (Phase 2) | In | Always `200 {}`; wrong client `403 unauthorized_client` |

## v1 REST API (from `config/routes/api.rb`)

### Instances, applications, accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/instance` | none | — | implemented (Phase 2) | In | instance discovery, `REST::V1::InstanceSerializer` |
| GET | `/api/v2/instance` | none | — | implemented (Phase 2) | In | instance discovery v2, `REST::InstanceSerializer` |
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
| GET | `/api/v1/domain_blocks` | user | follow \| read \| read:blocks | implemented (Phase 3) | In | domain strings, ascending; domain-string cursors |
| PUT | `/api/v1/domain_blocks` | user | follow \| write \| write:blocks | implemented (Phase 3) | In | `200 {}` |
| DELETE | `/api/v1/domain_blocks/:domain` | user | follow \| write \| write:blocks | implemented (Phase 3) | In | `200 {}`, idempotent |

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
| GET | `/api/v1/statuses/:id/context` | optional | read \| read:statuses | implemented (Phase 3) | In | 4096/4096 caps authenticated; 40/60 + depth 20 anonymous |
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
| POST | `/api/v1/statuses/:id/mute` | user | write:mutes | out of profile (Phase 3) | Out | conversation mute; `conversations` excluded |
| POST | `/api/v1/statuses/:id/unmute` | user | write:mutes | out of profile (Phase 3) | Out | as above |
| POST | `/api/v1/statuses/:id/translate` | — | — | out of profile | Out | translation provider not configured |

### Polls

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/polls/:id` | optional | read \| read:statuses | implemented (Phase 3) | In | 404 when missing/invisible |
| POST | `/api/v1/polls/:id/votes` | user | write \| write:statuses | implemented (Phase 3) | In | `choices[]` required (else 400); single-choice re-vote 422 |

### Timelines, media, notifications, discovery (later phases)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/timelines/home` | user | read | planned (Phase 4) | In | |
| GET | `/api/v1/timelines/public` | optional | read | planned (Phase 4) | In | |
| GET | `/api/v1/timelines/tag` | optional | read | planned (Phase 4) | In | |
| GET | `/api/v1/timelines/list` | user | read | planned (Phase 4) | In | |
| GET | `/api/v1/timelines/link` | optional | read | planned (Phase 7) | In | |
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
| POST | `/api/v1/lists` | user | write | implemented (Phase 4) | In | title blank/long and the 50-list limit answer the exact 422 bodies |
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
| GET | `/api/v2/search` | optional | read:search | implemented (Phase 4) | In | local corpus only: accounts by handle/display name, hashtags by prefix, statuses by bounded recent scan (authenticated only, like `status_searchable?`); `resolve` without auth → pinned 401; `collections` always `[]` |
| GET | `/api/v1/streaming` | user | read | planned (Phase 5) | In | SSE/WebSocket |
| GET | `/api/v1/streaming/(*any)` | user | read | planned (Phase 5) | In | |

### Explicitly excluded / out of profile (admin, moderation, legacy, experimental)

- `GET /api/v1/oembed` — public oEmbed; not in initial profile (record as gap if clients need it).
- `namespace :v1_alpha` — experimental/legacy (`accounts/collections`, `collections/*`); explicitly **Out**.
- `annual_reports`, `announcements`, `conversations`, `donation_campaigns`, `suggestions` — **Out** until scoped.
- `GET /api/v1/custom_emojis` — **implemented (Phase 4)**: public index over local + enabled + picker-visible emoji (CLI-managed; no admin API in this milestone). Emoji embedding in status payloads is implemented; account-payload emoji remain `[]` (Phase 7).
- `GET /api/v1/blocks`, `GET /api/v1/mutes`, `GET /api/v1/favourites`, `GET /api/v1/bookmarks` — relation collection reads; **planned (Phase 7)**.
- `POST /api/v1/reports` — moderation; **Out** (admin/moderation excluded).
- `accounts/familiar_followers`, `identity_proofs`, `featured_tags`, `email_subscriptions`, `collections`, `tags` follow/feature, `followed_tags` — **planned (Phase 7)**.
- `namespace :admin` (all `admin/*` routes) — **Out** explicitly; moderation/security operations excluded from public-client milestone.
- `v2` admin accounts, `push/*`, `web/*` — **Out** unless scoped later.
- `translate` (statuses) — **Out** unless a translation provider is configured.

## Non-route contract items tracked separately

- IDs: decimal strings on the wire, numeric ordering internally (§4 of `birchtree.md`).
- Pagination: `Link` `next`/`prev` headers; cursors numeric `Id`s (status/account/interaction ids) except domain blocks (domain-string cursors). Cursor values are opaque to clients; they are swapped in place and other query params are preserved.
- Error shapes: `{"error": "..."}`; statuses 200/400/401/403/404/405?/422/429 where applicable. Two 404 bodies exist and are reproduced: `Not Found` (explicit `not_found`, status sub-routes) and `Record not found` (unrescued lookup, update/delete).
- Rate-limit headers, streaming event envelopes (Phase 5).
- Idempotency: `Idempotency-Key` header on `POST /api/v1/statuses` only (1-hour retention, per-account scoping — matches Mastodon's `SETEX 3600` namespace). Other write endpoints are naturally idempotent (unique rows) instead.

## Known Phase 3 deviations / gaps (honesty list)

- **JSON request bodies are not accepted** on REST write endpoints (form-urlencoded or empty bodies only; axum's `RawForm` 415 replaced by a lenient form parser). Mastodon accepts JSON parameter bodies. Planned with the Phase 7 request-parser sweep.
- **Tag objects** in status payloads use the tag name as `id` (no tag table yet); `history`/`following`/`featuring` are contract-shaped defaults. Real tag rows land with the Phase 7 tag surface.
- **Conversation mutes** (`POST /api/v1/statuses/:id/mute|unmute`) are out of profile with `conversations`; the status DTO's `muted` flag is emitted with its neutral `false` for authenticated viewers.
- **`application` on statuses** is shown whenever present (Mastodon's `user_shows_application?` preference defaults to true and is not stored yet).
- Media attachment DTOs render `url`/`preview_url` as `null` until the attachment is processed (Phase 4 upload + processing lifecycle are live) — contract-correct "not processed" state for queued/in-progress/failed rows.
- Media file serving (`GET /system/media_attachments/files/{*path}`) serves the blob bytes with the sniffed content type and `Cache-Control: max-age=86400, public`.

## Known Phase 4 media deviations / gaps (honesty list)

- **No transcoding toolchain (no ffmpeg)**: animated GIF uploads keep the wire type `image` (Mastodon would produce `gifv` + mp4); video/audio keep their original bytes untouched; `meta.small`/`preview_url` exist only when an image preview was generated; duration/frame rate/bitrate stay `null` (MP4 `tkhd` dimensions are read when present).
- **Blob keys are sharded by a per-upload random 16-hex name** (`{aa}/{bb}/{random16}/original.{ext}`), not the attachment id: bytes must be staged *before* the id-minting commit (`birchtree.md` §5), so the id cannot appear in its own key. The wire URLs are unaffected.
- **The upload size cap equals the multipart body limit** (`ApiState` carries no `Config`); `Config::validate` enforces `max_file_size ≤ max_body_size`, so the effective cap matches the default 16 MB. The 422 message formats the cap like Rails `number_to_human_size` ("16 MB", "1 KB", …).
- **`meta.original.size` currently renders the literal string `{width}x{height}`** — a pre-existing `dto.rs` bug outside the media slice's file set; flagged for the Phase 4 integration pass.
- **A non-numeric `focus` param answers `422 Validation failed: Focus is invalid`** instead of Ruby's `Float()` coercion to `0.0`; a blank focus is ignored (upload) or clears the point (update).
- **An image upload carrying a thumbnail answers "Thumbnail must be blank"** before the thumbnail's own content-type/size checks (Mastodon's model validates the thumbnail's content type first); the pinned messages are otherwise exact.
