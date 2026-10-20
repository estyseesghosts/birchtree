# docs/endpoint-manifest.md

Status: Draft

Route inventory from Mastodon v4.7.3 `config/routes/api.rb` (commit `8ff11c2b`), cross-checked against the public API docs where practical. One manifest row per method/path. Profile column: `In` = in the declared compatibility profile (a row whose phase is still `planned` is `In`); `Out` = explicitly excluded (absent at v4.7.3, experimental or legacy, or no provider configured). Status column: `implemented (Phase N)` = route exists with contract tests; `planned (Phase N)` = in profile but intentionally absent (404) until that phase — never a success-shaped stub.

Scope expansion (2026-10-17): the admin/moderation API, reports, Web Push, conversations, suggestions, announcements, trends, annual reports, donation campaigns, and the `/api/web/*` routes are `In` with planned phases (see `birchtree.md` §7). Their rows are in the sections below. Known inventory gaps and one route-shape discrepancy are listed at the end.

Auth scopes are Mastodon conventions from the source/docs. They are enforced centrally (Phase 2) at the route boundary: a missing/invalid token yields `401 {"error":"The access token is invalid"}`, an insufficient scope yields `403 {"error":"This action is outside the authorized scopes"}`, and a user endpoint reached with an app-only token yields `422 {"error":"This method requires an authenticated user"}` (or `403 {"error":"This method requires an client credentials authentication"}` for the reverse case on registration). Scope lists are "any-of" (broad OR granular), matching Doorkeeper's `any?`. Admin routes additionally require a moderator-or-admin role (Mastodon's `Admin::PermissionsConcern`), checked before the scope check; a missing role yields `403 {"error":"Forbidden"}` (upstream `ErrorResponses#forbidden`). Roles (`admin`, `moderator`) are CLI-granted; moderator covers the `accounts` and `reports` admin areas, admin covers all.

## OAuth / discovery (public, not in config/routes/api.rb)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/.well-known/webfinger` | none | — | implemented (Phase 6, fix round) | In | WebFinger `acct:` resolution; suspended accounts → 410 (upstream `gone`) |
| GET | `/.well-known/nodeinfo` | none | — | implemented (Phase 6) | In | NodeInfo discovery |
| GET | `/nodeinfo/2.0` | none | — | implemented (Phase 6, fix round) | In | NodeInfo 2.0 document (real local-only `usage`, D31; `metadata` carries `nodeName`/`nodeDescription`; `software.name` is the `birchtree` rebrand, D48) |
| GET/POST | `/oauth/authorize` | session | — | implemented (Phase 2) | In | Doorkeeper authorize; login + consent rendered server-side |
| POST | `/oauth/token` | basic/client | — | implemented (Phase 2) | In | `authorization_code` (PKCE S256) + `client_credentials` |
| POST | `/oauth/revoke` | basic/client | — | implemented (Phase 2) | In | Always `200 {}`; wrong client `403 unauthorized_client` |

## v1 REST API (from `config/routes/api.rb`)

### Instances, applications, accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/instance` | none | — | implemented (Phase 2) | In | instance discovery, `REST::V1::InstanceSerializer` |
| GET | `/api/v2/instance` | none | — | implemented (Phase 2) | In | instance discovery v2, `REST::InstanceSerializer` |
| GET | `/api/v1/instance/peers` | none | — | implemented (fix round) | In | distinct remote domains, sorted; 404 when `peers_api_enabled` is off (default on) |
| GET | `/api/v1/instance/rules` | none | — | implemented (fix round) | In | server rules id-ordered, `translations:{}` (no translations stored) |
| GET | `/api/v1/instance/domain_blocks` | none | — | implemented (fix round) | In | 404 while `domain_blocks_public` is off (default, as upstream); serves silence/suspend rows with public digests when on |
| GET | `/api/v1/instance/languages` | none | — | implemented (fix round) | In | distinct status languages; `name` = code (no locale catalog — recorded divergence) |
| GET | `/api/v1/instance/activity` | none | — | implemented (fix round) | In | 12 trailing 7-day `{week, statuses, logins, registrations}` windows, string values; 404 when `activity_api_enabled` is off (default on) |
| GET | `/api/v1/instance/extended_description` | none | — | implemented (fix round) | In | unset shape `{updated_at:null,content:""}` (no authoring surface yet) |
| GET | `/api/v1/instance/translation_languages` | none | — | implemented (fix round) | In | `{}` (no translation provider configured) |
| GET | `/api/v1/instance/privacy_policy` | none | — | implemented (fix round) | In | honest 404: no policy text configured or bundled, so nothing is fabricated (`src/api/mastodon/endpoints/instance.rs:396-398`, `src/api/mastodon/router.rs:57`) |
| GET | `/api/v1/instance/terms_of_service` | none | — | implemented (fix round) | In | honest 404: no terms rows exist, matching upstream's unconfigured behavior (`src/api/mastodon/endpoints/instance.rs:406-408`, `src/api/mastodon/router.rs:55`) |
| GET | `/api/v1/instance/terms_of_service/:date` | none | — | implemented (fix round) | In | honest 404: every date is unknown with no terms rows (`src/api/mastodon/endpoints/instance.rs:414-416`, `src/api/mastodon/router.rs:56`) |
| GET | `/api/v1/accounts` | optional | read \| read:accounts | implemented (Phase 7) | In | multi-fetch `id[]` (up to 40; above that `422 Mastodon::ValidationError`); unapproved and pending-deletion accounts omitted (upstream `set_accounts`) |
| POST | `/api/v1/accounts` | app | write \| write:accounts | implemented (Phase 2) | In | registration; returns a user token |
| GET | `/api/v1/accounts/verify_credentials` | user | profile \| read \| read:accounts | implemented (Phase 2, source/role fix round) | In | `CredentialAccountSerializer` (real counters); `source` renders stored profile fields/discoverable/hide_collections/indexable/attribution (`null`/defaults when never set); `role` is real for staff (`Admin`/`Moderator` + permission bitmask), legacy `User` shape otherwise |
| PATCH | `/api/v1/accounts/update_credentials` | user | write \| write:accounts | implemented (Phase 2, fields fix round) | In | incl. `source[quote_policy]`. Accepts `source[chosen_languages][]` (D21; birchtree extension — upstream has no API setter): replaces the stored preference, empty clears it, absent leaves it unchanged. `bot`/`fields_attributes[]`/`attribution_domains[]` parsed with upstream rules (max 4, blank-label 422, truncation) and persisted (absent leaves stored values); `source[display_media|expand_spoilers|autoplay_gifs]` stored as a birchtree extension (upstream: web-settings-only). `avatar`/`header` need the media pipeline (named follow-up). CORE-L: top-level `default_license` (Pixelfed ids; present-blank clears, absent unchanged; ignored when `license_policy=off`; out-of-enum → 422 when enabled); account wire renders `preferredLicense` (number/`null`) only when enabled |
| GET | `/api/v1/accounts/lookup` | user | read | implemented (Phase 7, fix round) | In | leading-`@`-tolerant, domain-lowercased resolution incl. known remote accounts; suspended still returned; unknown remote → 404 (live webfinger = federation follow-up) |
| GET | `/api/v1/accounts/search` | user | read | implemented (Phase 7, fix round) | In | `require_user!` (was anonymous); default limit 40/max 80, `offset`; blank `q` → `[]`; exact handle match first (local + known remote + `resolve=`); `following=true` gates; suspended excluded |
| GET | `/api/v1/accounts/relationships` | user | read \| read:follows | implemented (Phase 2, real data Phase 3, fix round) | In | real per-viewer relationship aggregates; `without_suspended` unless truthy `with_suspended` |
| GET | `/api/v1/accounts/:id` | none | — | implemented (Phase 2, real counters Phase 3) | In | public profile |
| GET | `/api/v1/accounts/:id/statuses` | optional | read | implemented (Phase 3) | In | `pinned`/`exclude_replies`/`exclude_reblogs` filters, `Link` cursors |
| GET | `/invite/:invite_code` | none | — | implemented (round 3, durable round 4) | In | mirrors `Api::V1::InvitesController#show`: 200 `{invite_code, instance_api_url}`, 404 unknown, 401 `{error: "Invalid invitation code"}` for expired/exhausted (checked before the registrations gate); durable `INVITES` table (see D12) |
| POST | `/api/v1/emails/confirmations` | user | write \| write:accounts | implemented (round 3, delivers via job when configured) | In | re-sends the confirmation mail (`200 {}`); 403 when already confirmed or owned by another app (grandfathered pre-attribution rows allow any app); explicit 503 without a mail provider, never fake success; stores a random per-request bearer then enqueues a durable `SendConfirmationEmail` job (worker delivers after commit); redemption at `GET /auth/confirmation` (D12) |
| GET | `/api/v1/emails/check_confirmation` | user | read \| read:accounts | implemented (round 3) | In | bool; gated on unconfirmed (D12 remainder as above) |
| POST | `/api/v1/emails/password_reset` | none | — | implemented (round 4, public email flow) | In | no upstream API route (web-only `/auth/password` upstream); public `email`-param flow, always `200 {}` (unknown addresses queue nothing — no enumeration oracle); explicit 503 without a provider; stores a random bearer then enqueues `SendPasswordResetEmail` (6h TTL; redemption flow still open, D12) |
| GET | `/auth/confirmation` | none (browser link) | — | implemented (round 4) | In | redemption page for the mailed bearer: 200 "Email confirmed" vs 422 "Confirmation failed" (unknown/used/wrong-kind indistinguishable); consumes the token single-use (D12) |
| GET | `/api/v1/accounts/:id/followers` | optional | read | implemented (Phase 3) | In | `REST::AccountSerializer[]`, `Link` cursors |
| GET | `/api/v1/accounts/:id/following` | optional | read | implemented (Phase 3) | In | |
| POST | `/api/v1/accounts/:id/follow` | user | follow \| write \| write:follows | implemented (Phase 3) | In | locked target → `requested:true`; `languages[]` sets the D23 per-follow language filter (absent keeps, present-but-empty clears; relationships render `languages`) |
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
| GET | `/api/v1/apps/verify_credentials` | app | — | implemented (Phase 7) | In | valid-token check only; `REST::ApplicationSerializer` without secret |

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
| POST | `/api/v1/statuses` | user | write \| write:statuses | implemented (Phase 3) | In | `quoted_status_id`/`quote_approval_policy`, `poll[...]`, `media_ids[]`, `Idempotency-Key` (1h replay); CORE-L: `license_policy=enforced` requires an effective license (per-attachment else author default) on top-level non-DM statuses with media (422 otherwise; replies/DMs exempt; same gate on PUT edits that change `media_ids`) |
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
| GET | `/api/v1/statuses/:id/quotes` | user | read \| read:statuses | implemented (Phase 3, cursor fix round) | In | 401 without token; accepted quotes only; cursor = quote id (upstream `paginate_by_max_id`; rekeyed + migrated in schema v16) |
| POST | `/api/v1/statuses/:id/quotes/:quoting_status_id/revoke` | user | write \| write:statuses | implemented (Phase 3) | In | quoted author only; 200 = quoting status |
| POST | `/api/v1/statuses/:id/mute` | user | write \| write:mutes | implemented (Phase 7) | In | conversation mute; the status DTO `muted` reflects it |
| POST | `/api/v1/statuses/:id/unmute` | user | write \| write:mutes | implemented (Phase 7) | In | as above |
| POST | `/api/v1/statuses/:id/translate` | — | — | out of profile | Out | translation provider not configured |

### Polls

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/polls/:id` | optional | read \| read:statuses | implemented (Phase 3) | In | 404 when missing/invisible |
| POST | `/api/v1/polls/:id/votes` | user | write \| write:statuses | implemented (Phase 3, vote-semantics fix round) | In | missing `choices` → 400; present-but-empty → 200 no-op (upstream early return); multiple-choice re-votes merge additively (never replace/delete); divergences recorded: JSON `{"choices":[]}` flattens to missing (400), `choices[]=abc` → 422 (upstream 500s) |

### Timelines, media, notifications, discovery (later phases)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/timelines/home` | user | read \| read:statuses | implemented (Phase 4) | In | own statuses + the most recent 1000 active follows, minus members of the viewer's exclusive lists (D10b, Phase 4.1); `limit` 20/max 40 (`limit=0`/non-numeric → empty, no `Link`); `local` accepted and ignored (as upstream v4.7.3); app-only token → 422; `Cache-Control: private, no-store` |
| GET | `/api/v1/timelines/public` | optional | read \| read:statuses | implemented (Phase 4, feed-gate fix round) | In | public visibility only; self-replies kept, replies to others and reblogs dropped; `local`/`remote`/`only_media`; anonymous → `max-age=15, public, stale-while-revalidate=30, stale-if-error=86400`, authenticated → `private, no-store`. `*_live_feed_access` gates (default `public`): non-`public` requires login (anonymous → 401); `disabled` serves `[]` to non-staff |
| GET | `/api/v1/timelines/tag/{tag}` | optional | read \| read:statuses | implemented (Phase 4, any/all/none + feed-gate fix round) | In | tag normalized (case-insensitive, leading `#` stripped); unknown or empty tag → `200 []` with no `Link`; public visibility only; replies kept; reblog wrappers not indexed (matches upstream, see deviations); `local`/`remote`/`only_media`; `any[]`/`all[]`/`none[]` multi-tag queries (upstream `TagFeed` modes, 4-tag cap each); `*_topic_feed_access` gates as `public` above; cache header as `public` above |
| GET | `/api/v1/timelines/list/{id}` | user | read \| read:lists | implemented (Phase 4) | In | members' statuses under the home gates; the list's `replies_policy` (`list`/`followed`/`none`) governs replies; not owned or unparsable id → 404 `{"error":"Record not found"}`; app-only token → 422; `Cache-Control: private, no-store` |
| GET | `/api/v1/timelines/link` | optional | read | implemented (Phase 7) | In | link timeline over `PreviewCard` URLs (`?url=`) |
| GET | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | 206 while unprocessed; 422 on processing failure (Mastodon's controller authorizes write on every action, `show` included) |
| POST | `/api/v1/media` | user | write \| write:media | implemented (Phase 4, stills extended round 3) | In | media v1 (multipart, inline processing → 200); `image/heic`, `image/heif`, `image/tiff` (+`image/x-tiff`) accepted as stills → WebP via `ImageToWebp` (D4); undecodable input commits `Failed` (422), never served raw; CORE-L: optional `license` (Pixelfed ids 1,5,6,11-16; out-of-enum → 422 unless `license_policy=off`, which ignores it; wire omits `license` when off, renders number/`null` when enabled) |
| PUT | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | description/focus/thumbnail; 206 while unprocessed; CORE-L: optional `license` (present replaces, present-blank clears, absent unchanged; same off/enabled rule as upload) |
| DELETE | `/api/v1/media/:id` | user | write \| write:media | implemented (Phase 4) | In | 422 while attached to a status |
| POST | `/api/v2/media` | user | write \| write:media | implemented (Phase 4, stills extended round 3) | In | media v2 (multipart; video/audio delayed → 202, images inline → 200); HEIC/HEIF/TIFF accepted as stills → WebP (D4) |
| GET | `/api/v1/notifications` | user | read:notifications | implemented (Phase 4, params fix round) | In | `limit` 20/max 40 with `limit_param` semantics (`limit=0` → empty page); `types[]`/`exclude_types[]` accept all 17 upstream types (unbuilt-feature types parse but never match — register rows, not stubs); unknown values silently dropped; `account_id` filters by actor (fill-loop, cursor-stable); `include_filtered` accepted (no servable filtered rows yet); `supported_types[]` accepted (fallback rendering unbuilt); suspended/gone actors excluded (`without_suspended`); `group_key` uses the `ungrouped-<id>` fallback (no server-side merging in the v1 profile) |
| GET | `/api/v1/notifications/:id` | user | read:notifications | implemented (Phase 4) | In | 404 `{"error":"Record not found"}` for foreign/missing |
| POST | `/api/v1/notifications/clear` | user | write \| write:notifications | implemented (Phase 4) | In | `200 {}` |
| GET | `/api/v1/notifications/unread_count` | user | read:notifications | implemented (Phase 4) | In | `{"count": N}` = rows newer than the notifications marker (limit default 100, max 1000) |
| POST | `/api/v1/notifications/:id/dismiss` | user | write \| write:notifications | implemented (Phase 4) | In | `200 {}`; repeat → 404 |
| GET | `/api/v1/notifications/requests` | user | read:notifications | implemented (Phase 4 index, live pipeline round 3) | In | filter → request queue → merge pipeline per upstream `NotifyService`/`UnfilterNotificationsWorker` (D20); filtered mentions/quotes file requests instead of notifications |
| GET | `/api/v1/notifications/requests/merged` | user | read:notifications | implemented (Phase 4, live round 3) | In | `{"merged": <bool>}` — true when no pending requests remain |
| GET | `/api/v1/notifications/requests/:id` | user | read:notifications | implemented (round 3) | In | 404 for foreign/missing |
| POST | `/api/v1/notifications/requests/:id/accept` | user | write:notifications | implemented (round 3) | In | merges the request into notifications and emits `notifications_merged` (payload `"1"`) only when rows actually flip (D20) |
| POST | `/api/v1/notifications/requests/:id/dismiss` | user | write:notifications | implemented (round 3) | In | silent (no event) |
| POST | `/api/v1/notifications/requests/accept` | user | write:notifications | implemented (round 3) | In | bulk accept (upstream `requests/accept`) |
| POST | `/api/v1/notifications/requests/dismiss` | user | write:notifications | implemented (round 3) | In | bulk dismiss |
| GET | `/api/v1/notifications/policy` | user | read:notifications | implemented (round 3) | In | upstream V1 policy singleton defaults (`private_mentions` + `limited_accounts` = filter, rest accept) |
| PUT | `/api/v1/notifications/policy` | user | write:notifications | implemented (round 3) | In | set `filter_*` flags; absent flags keep their value; `for_limited_accounts` not settable here, like upstream V1 |
| GET/POST | `/api/v1/markers` | user | read:statuses / write:statuses | implemented (Phase 4) | In | `timeline[]` on GET; `home[last_read_id]`/`notifications[last_read_id]` on POST (query or body); response `{timeline:{last_read_id,version,updated_at}}`; fresh marker reports `version: 1` |
| GET | `/api/v1/preferences` | user | read \| read:accounts | implemented (Phase 2, reading-keys fix round) | In | incl. quote-policy defaults; `posting:default:language` cascades `default_language` → registration `locale`; `reading:*` render stored preferences (birchtree extension setters) defaulting to upstream defaults (`default`/`false`/`false`) |
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
| GET | `/api/v2/search` | optional | read:search | implemented (Phase 7.2) | In | accounts by handle/display name and hashtags by prefix on the database path in every mode; statuses switch by deploy-time configuration (`--search-mode=embedded` default / `database` / `off`, restart to change; authenticated only, like `status_searchable?`); cold/empty embedded index degrades to the database scan; historical unlisted rows join the index through the incremental job only; `resolve` without auth → pinned 401; `collections` always `[]` |
| GET | `/api/v1/streaming` | user | read \| read:statuses (user:notification: read \| read:notifications) | implemented (Phase 5) | In | WebSocket upgrade (subscribe by `?stream=` or message; any path under the prefix). Without an upgrade, `/api/v1/streaming` alone has no channel and answers `400 Unknown channel requested`. Host redirect when `--streaming-url` names another host (`301`). Contract: `docs/streaming-contract.md` |
| GET | `/api/v1/streaming/(*any)` | user | read \| read:statuses (user:notification: read \| read:notifications) | implemented (Phase 5) | In | SSE for `user`, `user/notification`, `public`, `public/local`, `public/remote` (`?only_media=true` for media), `hashtag`, `hashtag/local` (`?tag=`), `direct`, `list` (`?list=`); `GET …/health` answers `OK` unauthenticated. Refusals and texts: the contract. Public and hashtag streams apply the viewer's `chosen_languages` when set (D21). `direct` carries `conversation` events since Phase 7 (D18, closed); `public/remote` emits remote statuses since federation (D19, closed round 3) |

### Conversations (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/conversations` | user | read \| read:statuses | implemented (Phase 7) | In | per-viewer conversation list; id = thread-root status id |
| DELETE | `/api/v1/conversations/:id` | user | write \| write:conversations | implemented (Phase 7) | In | per-viewer state |
| POST | `/api/v1/conversations/:id/read` | user | write \| write:conversations | implemented (Phase 7) | In | |
| POST | `/api/v1/conversations/:id/unread` | user | write \| write:conversations | implemented (Phase 7) | In | |

### Suggestions (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/suggestions` | user | read \| read:accounts | implemented (fix round) | In | deprecated upstream (`Deprecation: @1621123200` only — no `Sunset` at the pin); `limit`/`offset`; honest local ranking (FoF then global fill, `SUGGESTION_MUTES` exclusions); app-only token → 422 |
| DELETE | `/api/v1/suggestions/:id` | user | write \| write:accounts | implemented (fix round) | In | `:id` is the suggested account id; persists the dismissal; `200 {}` (`render_empty`) |
| GET | `/api/v2/suggestions` | user | read \| read:accounts | implemented (fix round) | In | `REST::SuggestionSerializer[]` (`source` legacy map, `sources`); same ranking; FASP-sourced entries out of profile until a FASP integration exists; never fabricated |
| DELETE | `/api/v2/suggestions` | — | — | absent (fix round) | In | no such route upstream (`config/routes/api.rb:265`); Axum answers 405 on the GET path where Rails answers 404 |
| GET | `/api/v1/directory` | optional | read \| read:accounts (authed only) | implemented (fix round) | In | anonymous list; `local`/`order` (`new`/`active`)/`offset`/`limit`; always enabled (no `Setting.profile_directory` gate here); authed viewers exclude blocked/muted |
| GET/POST | `/oauth/userinfo` | user | profile | implemented (fix round) | In | OIDC claims (`sub`, `preferred_username`, `profile`, `picture`); wrong scope → 403, anonymous → 401 |

### Announcements (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/announcements` | user (any scope) | — | implemented (fix round) | In | `require_user!` only (no scope check); published announcements, chronological; durable rows (schema v14) |
| POST | `/api/v1/announcements/:id/dismiss` | user | write \| write:accounts | implemented (fix round) | In | `200 {}` (`render_empty`); 404 unless published |
| PUT | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | implemented (fix round) | In | `:id` is the reaction name; `200 {}`; 404 unless published; 422 on invalid emoji; max 8 distinct names; PUT is idempotent |
| PATCH | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | implemented (fix round) | In | alias of PUT |
| DELETE | `/api/v1/announcements/:announcement_id/reactions/:id` | user | write \| write:favourites | implemented (fix round) | In | removes the viewer's reaction by name; 404 when missing |
| GET/POST | `/api/v1/admin/announcements` | admin | admin:read / admin:write | implemented (fix round) | In | new API surface (upstream authoring is HTML-only); administration-only, Moderator refused; scheduled publish is manual (no publish job yet) |
| GET/PUT/PATCH/DELETE | `/api/v1/admin/announcements/:id` | admin | admin:read / admin:write | implemented (fix round) | In | as above; update never flips `published` by itself |
| POST | `/api/v1/admin/announcements/:id/publish` | admin | admin:write | implemented (fix round) | In | as above |
| POST | `/api/v1/admin/announcements/:id/unpublish` | admin | admin:write | implemented (fix round) | In | as above |

### Donation campaigns (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/donation_campaigns` | user (any scope) | — | implemented (fix round) | In | honest absence: outbound campaign fetch unbuilt, so 404 (`Record not found`) — never a success-shaped 204 |

### Web Push (Phase 7)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/push/subscription` | user | push | implemented (Phase 7, encrypted payloads round 3+4, fix round) | In | subscription bound to the access token; VAPID keys from configuration; standard subs get RFC 8291 `aes128gcm` (two-stage KDF, RFC-vector-locked), legacy keyed subs get draft-04 `aesgcm` (upstream selection rule), keyless legacy keep the tickle (D42: `access_token`/`icon` empty, never fabricated). Delivery retries then quarantines (4xx-except-408/429 destroy, D28 4xx-retry stands); VAPID `exp` 24h; `Unsubscribe-URL` sent when the subscription carries one (minted at creation; pre-token rows send none) |
| GET | `/api/v1/push/subscription` | user | push | implemented (Phase 7) | In | |
| PUT | `/api/v1/push/subscription` | user | push | implemented (Phase 7) | In | update; `check_push_subscription` guard |
| PATCH | `/api/v1/push/subscription` | user | push | implemented (Phase 7) | In | alias of PUT |
| DELETE | `/api/v1/push/subscription` | user | push | implemented (Phase 7) | In | |

### `/api/web/*` (Phase 7)

These routes serve keystodon's first-party mode. Upstream guards them with the Devise session and CSRF (`protect_from_forgery with: :exception`), not OAuth bearer tokens. birchtree's plan does the same (Phase 7 prerequisites in `birchtree.md` §7). Third-party clients never use them. Under the proposed default, first-party `/api/v1/*` calls use OAuth tokens (`docs/compatibility-profile.md`).

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| PUT | `/api/web/settings` | session + CSRF | — | implemented (Phase 7) | In | web UI settings sync; `require_user!` |
| PATCH | `/api/web/settings` | session + CSRF | — | implemented (Phase 7) | In | alias of PUT |
| GET | `/api/web/embeds/:id` | none | — | implemented (Phase 7) | In | local status: OEmbed JSON, `show?` authorization. Remote fetch/sanitize pass not built yet: signed-in remote reads answer 404 (fetch-failure equivalent) |
| POST | `/api/web/push_subscriptions` | session + CSRF | — | implemented (Phase 7) | In | `require_user!` |
| PUT | `/api/web/push_subscriptions/:id` | session + CSRF | — | implemented (Phase 7) | In | `require_user!` |
| PATCH | `/api/web/push_subscriptions/:id` | session + CSRF | — | implemented (Phase 7) | In | alias of PUT |
| DELETE | `/api/web/push_subscriptions/:id` | token (exempt) or session + CSRF | — | implemented (Phase 7) | In | no `require_user!` guard upstream (destroy only); the unsubscribe-token path is CSRF-exempt like upstream (`skip_forgery_protection only: :destroy`); the numeric-id fallback underneath is a birchtree extension and requires CSRF when a session user acts (D44) |

### Relation collection reads + tag surface (Phase 7)

Bullets in the old inventory that are now rows. Upstream sources: `app/controllers/api/v1/{blocks,mutes,favourites,bookmarks}_controller.rb`, `accounts/familiar_followers_controller.rb` (+ presenter), `featured_tags_controller.rb`, `followed_tags_controller.rb`, `tags_controller.rb` at v4.7.3.

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/blocks` | user | follow \| read \| read:blocks | implemented (Phase 7) | In | real read over BLOCKS; `Link` cursors |
| GET | `/api/v1/mutes` | user | follow \| read \| read:mutes | implemented (Phase 7) | In | real read over MUTES with `MutedAccountSerializer` (`mute_expires_at`) |
| GET | `/api/v1/favourites` | user | read \| read:favourites | implemented (Phase 7) | In | real read over FAVOURITES; `min_id` ignored (only `max_id`/`since_id` honored) |
| GET | `/api/v1/bookmarks` | user | read \| read:bookmarks | implemented (Phase 7) | In | real read over BOOKMARKS; `min_id` ignored (as above) |
| GET | `/api/v1/accounts/familiar_followers` | user | read \| read:accounts | implemented (Phase 7) | In | computed from real follows; input ids capped at 40 (upstream unbounded); unknown/suspended skipped; no `hide_collections` concept (no collections surface) |
| GET | `/api/v1/featured_tags` | user | read | implemented (Phase 7) | In | real rows + live usage counts from bounded tag scans |
| POST | `/api/v1/featured_tags` | user | write | implemented (Phase 7) | In | feature by name |
| DELETE | `/api/v1/featured_tags/:id` | user | write | implemented (Phase 7) | In | `:id` is the numeric featured-tag row |
| GET | `/api/v1/accounts/:id/featured_tags` | optional | read | implemented (Phase 7) | In | the account's featured tags |
| GET | `/api/v1/followed_tags` | user | read | implemented (Phase 7) | In | real read over TAG_FOLLOWS (limit 100); `min_id` ignored |
| GET | `/api/v1/tags/:id` | optional | read | implemented (Phase 7) | In | tag name as id (matches the recorded tag deviation); 404 when never used |
| POST | `/api/v1/tags/:id/follow` | user | write \| write:follows | implemented (Phase 7) | In | |
| POST | `/api/v1/tags/:id/unfollow` | user | write \| write:follows | implemented (Phase 7) | In | |
| POST | `/api/v1/tags/:id/feature` | user | write \| write:accounts | implemented (Phase 7) | In | |
| POST | `/api/v1/tags/:id/unfeature` | user | write \| write:accounts | implemented (Phase 7) | In | |
| GET | `/api/v1/custom_emojis` | none | — | implemented (Phase 4) | In | public index over local + enabled + picker-visible emoji (authored via CLI `add-emoji`/`remove-emoji`/`list-emojis` and the admin API below). Status-payload `emojis` arrays serve local + remote emoji; account-payload emoji remain `[]` |

### Trends, public (Phase 7.3)

Upstream serves only admin-approved items (`Trends.*.query.allowed`) and returns nothing when trends are disabled (`Setting.trends`). Pagination is `offset` and `limit` (default 10), not cursors.

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/trends` | optional | — | implemented (Phase 7.3) | In | deprecated alias of `/api/v1/trends/tags` (`Deprecation` from 2022-03-30); `cache_if_unauthenticated!` |
| GET | `/api/v1/trends/tags` | optional | — | implemented (Phase 7.3; score-ordered D40) | In | approved tags, score-ordered (observed uses with day-scale decay, decided constants); seven real per-day `history` buckets; authenticated viewers get real `following`/`featuring`; `?locale=` accepted, ordering-inert (no per-tag locale data) |
| GET | `/api/v1/trends/links` | optional | — | implemented (Phase 7.3; crawler D40) | In | approved preview cards only; metadata crawled through the safe client (SSRF/size/time/redirect, 200 KB cap), `null` until the crawl lands — never fabricated; score-ordered by observed shares |
| GET | `/api/v1/trends/statuses` | optional | — | implemented (Phase 7.3; score-ordered D40) | In | approved statuses only; score-ordered by reblogs+favourites with day-scale decay; `?locale=` orders matching-language statuses first, never filters |

### Reports (Phase 7.3)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/reports` | user | write \| write:reports | implemented (Phase 6, params fix round) | In | creates a report (`require_user!`); `category` enum-gated (`rule_ids` force `violation`), `rule_ids` validated (unknown → 422), `forward_to_domains` lowercased (forwarded iff target's domain listed), `forward` honored for non-local targets, `collection_ids` numeric-validated — all persisted; remote forwarding as Flag is Phase 6 |

### Annual reports (Phase 7.3)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/annual_reports` | user | read \| read:accounts | implemented (Phase 7.3) | In | index of the viewer's annual reports |
| GET | `/api/v1/annual_reports/:id` | user | read \| read:accounts | implemented (Phase 7.3) | In | |
| POST | `/api/v1/annual_reports/:id/read` | user | write \| write:accounts | implemented (Phase 7.3) | In | marks the report read |
| POST | `/api/v1/annual_reports/:id/generate` | user | write \| write:accounts | implemented (Phase 7.3) | In | enqueues generation as a durable job |
| GET | `/api/v1/annual_reports/:id/state` | user | read \| read:accounts | implemented (Phase 7.3) | In | generation state |

### Admin API (Phase 7.3)

All admin rows: auth `user + role`. The role is `admin` (every admin route) or `moderator` (reports and account routes; the split is proposed in `birchtree.md` §7 Phase 7.3). Reads take `admin:read` (or the listed sub-scope). Writes take `admin:write` (or the listed sub-scope). Each row is gated by the moderator-or-admin role check before its scope check. Upstream sources: `app/controllers/api/v1/admin/*` and `app/controllers/api/v2/admin/accounts_controller.rb` at v4.7.3.

#### Accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/accounts` | user + role | admin:read \| admin:read:accounts | implemented (Phase 7.3, identity stores fix round) | In | `REST::Admin::AccountSerializer[]`; `locale`/`invite_request`/`created_by_application_id`/`invited_by_account_id` served from registration attribution (null when absent); `ip`/`ips` from password-login telemetry (recorded since schema v15); `ip` filter substring-matches recorded logins |
| GET | `/api/v1/admin/accounts/:id` | user + role | admin:read \| admin:read:accounts | implemented (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/accounts/:id` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3; worker D38) | In | suspends+disables immediately and enqueues the bounded restartable deletion batches plus the federated `Delete` of the actor in the same transaction (upstream: async worker). Revokes the account's tokens and closes its open streams (D24) |
| POST | `/api/v1/admin/accounts/:id/enable` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3) | In | local accounts only (`require_local_account!`). Does not restore tokens revoked by an earlier kill (D24) |
| POST | `/api/v1/admin/accounts/:id/unsensitive` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/accounts/:id/unsilence` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/accounts/:id/unsuspend` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3) | In | enqueues unsuspension (upstream: async worker). Does not restore tokens revoked by an earlier kill (D24) |
| POST | `/api/v1/admin/accounts/:id/approve` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3, queue round 2) | In | local accounts only; pending-only (`403 Forbidden` when already approved); sets `approved=true` + re-enables; needs `--registration-require-approval` to ever see pending accounts. Does not restore tokens revoked by an earlier kill (D24) |
| POST | `/api/v1/admin/accounts/:id/reject` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3, queue round 2) | In | local accounts only; pending-only (`403 Forbidden` when already approved); suspends+disables with token kill (upstream deletes — aligns with the D38 worker); answers `200 {}` |
| POST | `/api/v1/admin/accounts/:account_id/action` | user + role | admin:write \| admin:write:accounts | implemented (Phase 7.3, warnings round 2) | In | account action (`Admin::AccountAction`); the nested path parameter is `:account_id`; every type records a warning row (`text` stored; `none` without `report_id` → 422; `warning_preset_id` → 404; warning e-mail never sent). `disable` and `suspend` revoke the account's tokens and close its open streams (D24); other action types leave tokens alone |

#### Reports (moderator queue)

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/reports` | user + role | admin:read \| admin:read:reports | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/reports/:id` | user + role | admin:read \| admin:read:reports | implemented (Phase 7.3) | In | |
| PUT | `/api/v1/admin/reports/:id` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | update; `rule_ids[]` validated against the rule store (unknown → 422); admin report `rules` render `[{id, text}]` |
| PATCH | `/api/v1/admin/reports/:id` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | alias of PUT; `rule_ids[]` validated as above |
| POST | `/api/v1/admin/reports/:id/assign_to_self` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/unassign` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/reopen` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/reports/:id/resolve` | user + role | admin:write \| admin:write:reports | implemented (Phase 7.3) | In | |

#### Custom emojis

New API surface (no upstream REST equivalent in v4.7.3 — upstream authoring is the HTML `Admin::CustomEmojisController`): admins-only role gate (`AdminArea::AdminOnly`; moderators refused like the announcements module), bare `admin:read`/`admin:write` scopes (upstream defines no custom-emoji sub-scopes).

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/custom_emojis` | user + role | admin:read | implemented (emoji round) | In | every local row, shortcode order, incl. disabled + picker-hidden; rows render with the shared serializer plus a `disabled` flag |
| POST | `/api/v1/admin/custom_emojis` | user + role | admin:write | implemented (emoji round) | In | multipart `shortcode` + `image` (+ optional `category`, `visible_in_picker`); 200 + row; re-post replaces bytes and metadata; 422 on blank/bad shortcode, >256 KB, or non-PNG/GIF/WebP bytes |
| PUT | `/api/v1/admin/custom_emojis/:shortcode` | user + role | admin:write | implemented (emoji round) | In | JSON `category` (string-or-null) / `visible_in_picker` / `disabled`; 200 + row; 404 when missing; 422 on empty or wrong-typed body |
| DELETE | `/api/v1/admin/custom_emojis/:shortcode` | user + role | admin:write | implemented (emoji round) | In | removes the row and its image bytes; 404 when missing; 200 `{}` |

#### Domain allows, domain blocks, email-domain blocks, IP blocks

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/domain_allows` | user + role | admin:read \| admin:read:domain_allows | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/domain_allows/:id` | user + role | admin:read \| admin:read:domain_allows | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/domain_allows` | user + role | admin:write \| admin:write:domain_allows | implemented (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/domain_allows/:id` | user + role | admin:write \| admin:write:domain_allows | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/domain_blocks` | user + role | admin:read \| admin:read:domain_blocks | implemented (Phase 7.3) | In | server-level blocks; enforced at federation ingress and egress (Phase 6) |
| GET | `/api/v1/admin/domain_blocks/:id` | user + role | admin:read \| admin:read:domain_blocks | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/domain_blocks` | user + role | admin:write \| admin:write:domain_blocks | implemented (Phase 7.3) | In | |
| PUT | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | implemented (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | implemented (Phase 7.3) | In | alias of PUT |
| DELETE | `/api/v1/admin/domain_blocks/:id` | user + role | admin:write \| admin:write:domain_blocks | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/email_domain_blocks` | user + role | admin:read \| admin:read:email_domain_blocks | implemented (Phase 7.3) | In | registration-time enforcement needs email confirmation (Phase 2 gap) |
| GET | `/api/v1/admin/email_domain_blocks/:id` | user + role | admin:read \| admin:read:email_domain_blocks | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/email_domain_blocks` | user + role | admin:write \| admin:write:email_domain_blocks | implemented (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/email_domain_blocks/:id` | user + role | admin:write \| admin:write:email_domain_blocks | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/ip_blocks` | user + role | admin:read \| admin:read:ip_blocks | implemented (Phase 7.3) | In | enforcement needs a request-path check (trusted-proxy client IP, Phase 1) |
| GET | `/api/v1/admin/ip_blocks/:id` | user + role | admin:read \| admin:read:ip_blocks | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/ip_blocks` | user + role | admin:write \| admin:write:ip_blocks | implemented (Phase 7.3) | In | |
| PUT | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | implemented (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | implemented (Phase 7.3) | In | alias of PUT |
| DELETE | `/api/v1/admin/ip_blocks/:id` | user + role | admin:write \| admin:write:ip_blocks | implemented (Phase 7.3) | In | |

#### Trends approval

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/trends/tags` | user + role | admin:read | implemented (Phase 7.3) | In | no sub-scope upstream |
| POST | `/api/v1/admin/trends/tags/:id/approve` | user + role | admin:write | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/tags/:id/reject` | user + role | admin:write | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/links` | user + role | admin:read | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/:id/approve` | user + role | admin:write | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/:id/reject` | user + role | admin:write | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/links/publishers` | user + role | admin:read | implemented (Phase 7.3) | In | preview card providers (`path: :publishers` upstream) |
| POST | `/api/v1/admin/trends/links/publishers/:id/approve` | user + role | admin:write | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/links/publishers/:id/reject` | user + role | admin:write | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/trends/statuses` | user + role | admin:read | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/statuses/:id/approve` | user + role | admin:write | implemented (Phase 7.3) | In | |
| POST | `/api/v1/admin/trends/statuses/:id/reject` | user + role | admin:write | implemented (Phase 7.3) | In | |

#### Metrics

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| POST | `/api/v1/admin/measures` | user + role (admin or moderator, fix round) | admin:read | implemented (Phase 7.3, keys round 2, extended round 3) | In | POST with a read scope upstream (kept). Served: `new_users`, `opened_reports`, `resolved_reports`, `active_users` (password-login telemetry), `instance_follows`, `instance_followers`, `instance_statuses`, `instance_reports`, `instance_accounts`, `instance_media_attachments`, `tag_accounts`/`tag_uses`/`tag_servers` (need `<key>[id]>`; omitted without a resolvable id, never zero-filled), `interactions` (unified journal: non-self replies, created favourites/reblogs, votes, new follows/requests). Omitted with reasons (never fabricated): `software_versions` (no version census) |
| POST | `/api/v1/admin/dimensions` | user + role (admin or moderator, fix round) | admin:read | implemented (Phase 7.3, keys round 2, extended round 3) | In | POST with a read scope upstream. Served: `languages`, `sources`, `servers`, `space_usage` (media + measured redb `database` bytes; no postgres/redis/search), `instance_accounts`, `instance_languages` (bare-code `human_key`), `tag_servers`/`tag_languages` (need `<key>[id]>`). Omitted with reasons: `software_versions` (no version census) |
| POST | `/api/v1/admin/retention` | user + role (admin or moderator, fix round) | admin:read | implemented (Phase 7.3, cohorts round 3) | In | POST with a read scope upstream (kept). Cohorts are sign-in-unioned-with-authorship (superset of upstream sign-in-only; pure sign-in would break the authorship cohort test) |

#### Canonical email blocks and tags

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v1/admin/canonical_email_blocks` | user + role | admin:read \| admin:read:canonical_email_blocks | implemented (Phase 7.3) | In | registration-time enforcement needs email confirmation (Phase 2 gap) |
| POST | `/api/v1/admin/canonical_email_blocks/test` | user + role | admin:read \| admin:read:canonical_email_blocks | implemented (Phase 7.3) | In | read scope upstream (`test` is in the read guard) |
| POST | `/api/v1/admin/canonical_email_blocks` | user + role | admin:write \| admin:write:canonical_email_blocks | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/canonical_email_blocks/:id` | user + role | admin:read \| admin:read:canonical_email_blocks | implemented (Phase 7.3) | In | |
| DELETE | `/api/v1/admin/canonical_email_blocks/:id` | user + role | admin:write \| admin:write:canonical_email_blocks | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/tags` | user + role | admin:read | implemented (Phase 7.3) | In | |
| GET | `/api/v1/admin/tags/:id` | user + role | admin:read | implemented (Phase 7.3) | In | |
| PUT | `/api/v1/admin/tags/:id` | user + role | admin:write | implemented (Phase 7.3) | In | update |
| PATCH | `/api/v1/admin/tags/:id` | user + role | admin:write | implemented (Phase 7.3) | In | alias of PUT |

#### v2 admin accounts

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/api/v2/admin/accounts` | user + role | admin:read \| admin:read:accounts | implemented (Phase 7.3, identity stores fix round) | In | v1 admin accounts index with filter parameters (`origin`, `status`, `permissions`, `username`, `by_domain`, `display_name`, `email`, `ip`, `invited_by`, `role_ids`); `invited_by` resolves through invite attribution (garbage → empty page, as upstream); `Link` cursors (`max_id`, `min_id`) |

### Bullet-only entries (excluded, or not yet rowed)

Rowed since the 2026-10-17 re-scope (each has a row above with its phase): `annual_reports`, `announcements` (+ admin authoring), `conversations` (+ mutes), `donation_campaigns`, `suggestions` (+ directory, userinfo), `POST /api/v1/reports`, `namespace :admin`, v2 admin accounts, `push/*`, `web/*`, `GET /api/v1/accounts` (multi-fetch), `directory`, the instance sub-resources (`peers`, `rules`, `domain_blocks`, `terms_of_service` + `:date`, `privacy_policy`, `extended_description`, `translation_languages`, `languages`, `activity`), `emails/confirmations` + `check_confirmation` + `password_reset` + `GET /auth/confirmation`, domain blocks (index/create/destroy), `tags` follow/unfollow/feature/unfeature + `followed_tags` + featured tags (top-level and per-account), and the notification-requests + `notifications/policy` surface. The entries below remain as bullets because they are not yet rowed or are excluded.

- `GET /api/v1/oembed` — public oEmbed; not in initial profile (record as gap if clients need it). **Out.**
- `namespace :v1_alpha` — experimental/legacy (`accounts/collections`, `collections/*`); explicitly **Out**.
- `GET /api/v1/custom_emojis` — rowed above (was listed here for historical reasons).
- `GET /api/v1/blocks`, `GET /api/v1/mutes`, `GET /api/v1/favourites`, `GET /api/v1/bookmarks` — relation collection reads; **implemented (Phase 7)** (rows above).
- `accounts/familiar_followers`, `featured_tags`, `tags` follow/feature, `followed_tags` — **implemented (Phase 7)** (rows above). Still out: `identity_proofs` (needs cryptographic proof verification), `email_subscriptions`, `collections` (experimental `v1_alpha`).
- `translate` (statuses) — **Out** unless a translation provider is configured (row above).

## Non-route contract items tracked separately

- IDs: decimal strings on the wire, numeric ordering internally (§4 of `birchtree.md`).
- Pagination: `Link` `next`/`prev` headers; cursors numeric `Id`s (status, account, interaction and domain-block row ids). Cursor values are opaque to clients; they are swapped in place and other query params are preserved. Trends use `offset`/`limit` instead (Phase 7.3).
- Error shapes: `{"error": "..."}`; statuses 200/400/401/403/404/405?/422/429 where applicable. Two 404 bodies exist and are reproduced: `Not Found` (explicit `not_found`, status sub-routes) and `Record not found` (unrescued lookup, update/delete).
- Rate-limit headers. Streaming event envelopes (Phase 5, implemented): `event:`/`data:` over SSE, `{"stream","event","payload"}` over WebSocket; see `docs/streaming-contract.md`.
- Deprecation headers: `Deprecation` (`@<unix timestamp>`) and, where upstream sets it, `Sunset`. Applies to `GET /api/v1/suggestions` and `GET /api/v1/trends` (Phase 7 / 7.3).
- Async-refresh header: `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]` (Phase 6 remote thread completion; Phase 7 suggestions FASP path).
- Idempotency: `Idempotency-Key` header on `POST /api/v1/statuses` only (1-hour retention, per-account scoping — matches Mastodon's `SETEX 3600` namespace). Other write endpoints are naturally idempotent (unique rows) instead.
- Admin scope families: `admin:read` and `admin:write`, with per-resource sub-scopes (`admin:read:accounts`, `admin:write:reports`, and so on), as registered by the pinned Doorkeeper configuration. In birchtree's scope registry since Phase 7.3 (trends/tags/measures/dimensions/retention use the bare scopes upstream, with no sub-scopes).

## Known Phase 3 deviations / gaps (honesty list)

- **JSON request bodies: resolved in Phase 4.1 (D11, 2026-10-17).** Every REST write endpoint that reads a body accepts `application/json` as well as form and empty bodies. See "Phase 4.1: JSON bodies and exclusive lists" below. The Phase 3 wording, "not accepted", no longer applies.
- **Tag objects** in status payloads use the tag name as `id` (no tag table yet); `history`/`following`/`featuring` are contract-shaped defaults. Phase 7 added real tag follows/featured rows (D25 unified `#café`/`#cafe`); tag-row identity stays name-based.
- **Conversation mutes** (`POST /api/v1/statuses/:id/mute|unmute`) shipped with `conversations` (Phase 7): the status DTO's `muted` flag reflects the viewer's conversation mute.
- **`application` on statuses** is shown whenever present (Mastodon's `user_shows_application?` preference defaults to true and is not stored yet).
- Media attachment DTOs render `url`/`preview_url` as `null` until the attachment is processed (Phase 4 upload + processing lifecycle are live) — contract-correct "not processed" state for queued/in-progress/failed rows.
- Media file serving (`GET /system/media_attachments/files/{*path}`) serves the blob bytes with the sniffed content type and `Cache-Control: max-age=86400, public`.

## Known Phase 4 media deviations / gaps (honesty list)

- **Phase 7.1 shipped (2026-10-20):** ffmpeg-backed post-commit transcode; GIF → WebM/VP9 typed `video`, video → WebM/VP9 + Opus (VP9/WebM + Opus-or-silent within `max_file_size` passes through), stills → WebP (WebP passes through), `small` preview is WebP; metadata carried via `-map_metadata 0`, never stripped; `gifv` never produced (D1–D4, accepted). Round 3: `image/heic`, `image/heif`, `image/tiff` (+`image/x-tiff`) accepted as stills → WebP; undecodable input commits `Failed` (422). True HEIC→WebP byte proof needs an ffmpeg with hevc/heif decode on the verifying machine. v1 transcodes inline (`200`); v2 returns `202` for transcoded types, `GET /api/v1/media/:id` answers `206` until `Ready`, `422` on `Failed`. Evidence: `tests/phase7_1_transcode.rs`, `tests/round3_heic_tiff.rs`.
- **Blob keys are sharded by a per-upload random 16-hex name** (`{aa}/{bb}/{random16}/original.{ext}`), not the attachment id: bytes must be staged *before* the id-minting commit (`birchtree.md` §5), so the id cannot appear in its own key. The wire URLs are unaffected.
- **The upload size cap equals the multipart body limit** (`ApiState` carries no `Config`); `Config::validate` enforces `max_file_size ≤ max_body_size`, so the effective cap matches the default 16 MB. The 422 message formats the cap like Rails `number_to_human_size` ("16 MB", "1 KB", …).
- **A non-numeric `focus` param answers `422 Validation failed: Focus is invalid`** instead of Ruby's `Float()` coercion to `0.0`; a blank focus is ignored (upload) or clears the point (update).
- **An image upload carrying a thumbnail answers "Thumbnail must be blank"** before the thumbnail's own content-type/size checks (Mastodon's model validates the thumbnail's content type first); the pinned messages are otherwise exact.

## Search deviations (Phase 7.2 engine, 2026-10-20)

- **Local corpus for accounts/hashtags in every mode.** Status search switches by configuration: `embedded` (default, in-process index), `database` (the Phase 4 bounded-scan behavior, kept as a selectable backend), `off` (no status hits, matching upstream with no engine).
- **Upstream returns no status hits without a search engine** (`SearchService#status_searchable?` requires `Chewy.enabled?`). This server's `embedded` default returns hits (D6, accepted) while `off` matches upstream exactly; `database` keeps the interim scan (D7, kept as a selectable backend).
- **Index scope is public + unlisted only** (private/direct never indexed, never leave the database through the index). Indexing is a post-commit durable job; a bounded restartable backfill covers existing rows. The index is derived state (rebuildable, in-memory with a startup re-seed chain); when cold/empty, status search degrades to the database scan.
- **Backfill pages the public corpus only.** Historical unlisted statuses pre-dating the index join it through the incremental post-commit job only (re-indexed on edit/visibility-change, never on a timer); until the `indexable_statuses_page` port lands, a backfilled index under-serves old unlisted rows while the cold fallback (database scan) still finds them. New unlisted statuses are indexed at creation like public ones.
- **`resolve` (remote lookup)** depends on federation (Phase 6). Unauthenticated `resolve` returns the pinned 401.
- **`collections`** is always `[]`.
- Library evaluation (license, maintenance, fit): shipped a purpose-built in-process inverted index (zero new deps; Tantivy recorded as the future upgrade path behind the same backend port). Evidence: `tests/phase7_2_search.rs`.

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
- **Remote accounts exist since federation (D19, closed round 3).** `remote=true` serves federated remote statuses on the public timeline and `public:remote` streams; the domain-block rule applies to the viewer's domain blocks. Remote emoji ingest and remote counts remain federation-tail gaps.
- **`replies_policy=none` on lists** keeps only self-replies and replies to the viewer; `followed` keeps replies to followed parents; `list` keeps replies to members. Replies to the viewer pass under every policy.

## Phase 5: streaming (honesty list)

- **Source-derived, not run against upstream.** The stream names, scope rules, refusal texts, envelopes, heartbeat and redirect come from the pinned source (`streaming/index.js`, `streaming_controller.rb`, the fan-out and feed services at `8ff11c2b`). No live v4.7.3 instance was run: see the Phase 5 verification log.
- **Birchtree-only refusals:** `429 Too many streaming connections for this token`, `503 Too many streaming connections`, `429 Too many streaming subscriptions` (D14).
- **Not emitted:** `announcement`, `announcement.reaction`, `announcement.delete` (reads/dismissals/reactions are REST-only so far). `conversation` events emit on `direct` (D18, closed Phase 7); `kill` is sent on suspension/disablement (D24). `filters_changed` is never transmitted, as upstream never transmits it (no payload). `notifications_merged` is emitted on real merges only (round 3, D20).
- **Silent by design until their phases:** `direct` (D18, closed Phase 7).

## Route inventory discrepancies and gaps (found 2026-10-17)

Recorded, not fixed. Each needs a decision or a contract-review pass.

1. **Domain-block route shape: resolved 2026-10-17.** The Phase 3 rows now match the pinned tag: `GET`, `POST` and `DELETE /api/v1/domain_blocks`, with numeric row-id paging and the domain as a parameter. The correction is recorded in `birchtree.md` §7 Phase 3 item 6 and in `docs/logs/2026-10-17-domain-blocks-correction.md`.
2. **Pinned routes not yet rowed (narrowed fix round, 2026-10-20).** `directory`, the instance sub-resources, `emails/*`, domain blocks, `tags`/`followed_tags`/`featured_tags`, and the notification-requests + `policy` surface are rowed above and removed from this list. Each remaining item needs a row in Phase 7 or later:
   - `profile` (`GET`/`PATCH /api/v1/profile`, `DELETE` of avatar and header);
   - `peers/search` (instance peer search; `peers` itself is rowed above);
   - `domain_blocks/preview`;
   - accounts sub-resources: `lists`, `identity_proofs`, `email_subscriptions`, `collections`, `in_collections` still to row; `familiar_followers` already rowed above (Phase 7); top-level `collections` (`GET`/`POST`/`PUT`/`DELETE`, and `items`/`revoke`) still to row;
   - `GET /api/v2/notifications` and its `clear`, `unread_count`, `dismiss`, `:group_key/accounts`, and `policy` sub-routes.
3. **`GET /api/v1/accounts` (index) had no row.** Resolved in the 2026-10-17 follow-up: the row is in profile at Phase 7, as the account multi-fetch (`id[]`, up to 40).
4. **`namespace :web` and `/api/web/*` auth.** Resolved as a design decision in the 2026-10-17 follow-up: these routes serve keystodon's first-party mode and use the browser session plus CSRF (Phase 7). Still open: whether first-party `/api/v1/*` calls use OAuth tokens (proposed) or the session.
5. **Scope registry comment: resolved (fix round).** `KNOWN_SCOPES` lists the `admin:*` families (`src/api/oauth/scopes.rs:48-63`), so admin scopes validate and are grantable through the normal scope check; the old "accepted for storage but never granted / rejected today" wording no longer applies.
6. **`/api/v1/trends` and `/api/v1/trends/tags` in the pinned tag** serve the same action (`trends/tags#index`). Birchtree's rows keep both; the alias is deprecated upstream.
