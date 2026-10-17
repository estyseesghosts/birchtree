# docs/streaming-contract.md

Status: Draft (implemented in Phase 5, 2026-10-17; Frozen only on operator approval)

Streaming contract (per `birchtree.md` §2.6, §6 and §7 Phase 5). Upstream reference: Mastodon v4.7.3 (`8ff11c2b`), `streaming/index.js`, `app/controllers/api/v1/streaming_controller.rb`, `app/lib/feed_manager.rb`, `app/services/fan_out_on_write_service.rb`, `app/services/remove_status_service.rb`, `app/services/batched_remove_status_service.rb`, `app/workers/push_update_worker.rb`, `app/services/notify_service.rb`. Implementation: `src/streaming/` (adapter) and `src/application/streaming.rs` (events) with `src/application/timelines.rs` (the gates).

Streams are notifications of committed changes. They are not an event log. Nothing is persisted or replayed for a client that is not connected, and a reconnect does not recover missed events. This matches upstream's Redis publish/subscribe.

## Transports

**Server-Sent Events.** `GET /api/v1/streaming/{channel}` where `{channel}` is one of `user`, `user/notification`, `public`, `public/local`, `public/remote`, `hashtag`, `hashtag/local`, `direct`, `list`. Query parameters: `access_token` (optional credential), `only_media=true` (public family: the media variant), `tag` (hashtag, required), `list` (list id, required). The response is `200` with `Content-Type: text/event-stream` and `Cache-Control: private, no-store`. The body opens with the comment `:)` and carries a `:thump` comment every 15 seconds.

**WebSocket.** A WebSocket upgrade on any path under `/api/v1/streaming` (upstream's upgrade handler is path-agnostic). Subscriptions come from:

- the query string: `?stream=<name>` (and `tag` or `list`), subscribed before the upgrade response is returned;
- client messages: `{"type":"subscribe","stream":"<name>","tag":"…","list":"…"}` and `{"type":"unsubscribe",…}`.

Stream names are `user`, `user:notification`, `public`, `public:local`, `public:remote`, `public:media`, `public:local:media`, `public:remote:media`, `hashtag`, `hashtag:local`, `direct`, `list`. A WebSocket subscribes to several streams. A duplicate subscription (the same channel set, as upstream computes it) is ignored. Client messages that are not JSON objects are ignored.

**Health.** `GET /api/v1/streaming/health` answers `200` with the text `OK` (`text/plain`, `Cache-Control: private, no-store`) without authentication.

**Host discovery and redirect.** `urls.streaming` is `ws`/`wss` derived from the public URL, unless `--streaming-url` names another base. When that base is a different host from this server, every streaming path (`/api/v1/streaming` and `/api/v1/streaming/*`) answers `301` with `Location` set to the same scheme and path on the configured host. A request addressed to the configured host is served in-process. Upstream answers `404` for a same-host request, because its Node service sits behind the same origin; birchtree serves it instead (register D16).

## Authentication and scopes

- The token is taken from the `Authorization: Bearer` header; if absent, from the `access_token` query parameter; if absent, from the `sec-websocket-protocol` header value. Upstream's precedence.
- A user token is required for every stream, public ones included. An app-only (client-credentials) token is refused as an invalid token, because it has no user to stream for.
- A revoked token is refused, and so is a token whose application no longer exists.
- Scopes: `user`, `public*`, `hashtag*`, `direct` and `list` need `read` or `read:statuses`. `user:notification` needs `read` or `read:notifications`. The `user` stream carries notifications when the token has `read` or `read:notifications` (upstream's `channelsForUserStream`).
- A `list` stream needs a list the token's user owns.

Refusals:

| Condition | Status | Body or header |
|---|---|---|
| No token | 401 | `{"error":"Missing access token"}` (SSE); `X-Error-Message: Missing access token`, empty body (WebSocket upgrade) |
| Unknown or revoked token; app-only token | 401 | `Invalid access token` |
| Scopes do not admit the stream | 401 | `Access token does not have the required scopes` |
| Unknown SSE channel | 400 | `Unknown channel requested` (checked before authentication) |
| Unknown WebSocket stream name | 400 | error frame `{"error":"Unknown stream type","status":400}` (after the scope check, as upstream) |
| `hashtag` without `tag` | 400 | `Missing tag name parameter` |
| `list` without `list` | 400 | `Missing list name parameter` |
| `list` the user does not own, or a non-numeric list id | 401 | `Not authorized to stream this list` |
| Too many connections for the token | 429 | `Too many streaming connections for this token` |
| Too many connections in total | 503 | `Too many streaming connections` |
| Too many subscriptions on one WebSocket | 429 | error frame `{"error":"Too many streaming subscriptions","status":429}` |

A WebSocket subscribe refusal is a text frame `{"error":…,"status":…}`; the socket stays open. A WebSocket unsubscribe refusal is `{"error":"Error unsubscribing from channel"}` (no status), as upstream.

## Event envelopes

- **SSE:** `event: <name>` then `data: <payload>` then a blank line.
- **WebSocket:** `{"stream":[<name>, <parameter?>],"event":"<name>","payload":"<payload as a JSON string>"}`. The `stream` array is the subscription's name, followed by the `tag` or `list` parameter as the client sent it. The field order matches upstream's.

Payloads:

- `update`, `status.update`: the REST status DTO. A home, list or notification-channel payload is rendered for the subscriber (`favourited`, `reblogged`, `bookmarked`, `filtered` included). A public or hashtag payload is rendered anonymously (no per-viewer keys), and a logged-in subscriber gets a `filtered` array added. Upstream's `streamFrom` listener does the same.
- `delete`: the bare status id as a string, not JSON.
- `notification`: the REST notification DTO (the same rendering as `GET /api/v1/notifications`).

## Streams

| Stream | Carries | Gate (the same policy the REST surface applies) |
|---|---|---|
| `user` | `update`, `status.update`, `delete`; `notification` (with notifications in scope); `status.update` for an edit that mentions the viewer (upstream's notifications channel, folded in) | Home gate: own statuses; statuses from followed authors; direct only to mentioned viewers; reply rules; reblogs only where the follow shows reblogs and the target passes; exclusive-list members left out (D10b); blocks, mutes and domain blocks (author); `delete` on the pre-deletion snapshot |
| `user:notification` | `notification`; `status.update` for an edit that mentions the viewer | Recipient is the viewer. Birchtree does not drop notifications from blocked or muted accounts (upstream does; D26) |
| `public` | `update`, `status.update`, `delete` | Public visibility, not a reblog, the public reply rule (self-replies and top-level only); author not blocked, muted or domain-blocked; for a logged-in viewer, no mentioned account blocked or muted (upstream's streaming check). `delete` is sent without a viewer check, as upstream does |
| `public:local`, `public:remote` | as `public` | Origin: local is the author's account being local; remote is not. Remote is empty until Phase 6 (D19) |
| `public:media` and the local/remote media variants | as `public` | Only statuses with media attachments |
| `hashtag`, `hashtag:local` | `update`, `status.update`, `delete` | Public, not a reblog, carries the normalized tag (`normalize_tag`, as the REST tag timeline; not upstream's `normalizeHashtag`, D25); replies kept (upstream's hashtag streams keep them); author and mention block or mute check; `local` also requires a local author |
| `list` | `update`, `status.update`, `delete` | The list timeline's rules (`ListFilter`): members only, the list's replies policy, the home reblog rules; the owner's own statuses |
| `direct` | nothing in Phase 5 | Authorized and held open. Conversation events arrive with Phase 7 (D18) |

## What publishes, and when

Publication runs after a use case commits, in the adapter, never inside a storage transaction. An event carries an id where the hub can re-read durable state, and a snapshot where the object is gone (a deletion).

- **Status created** (`POST /api/v1/statuses`, scheduled publish job): `update` through the gates. An idempotent replay publishes nothing.
- **Status edited** (`PUT`/`PATCH /api/v1/statuses/:id`, only when the edit changed something): `status.update` through the gates; plus `status.update` on the notification channel of each active mentioned viewer.
- **Status deleted** (`DELETE`): `delete` for the status and every reblog wrapper the store's cascade tombstoned, each judged on its pre-deletion snapshot against every gate.
- **Reblog** (`POST …/reblog`) creates a wrapper: `update` for the wrapper through the home gate. A repeat reblog publishes nothing.
- **Unreblog**: `delete` for the viewer's wrapper.
- **Notification created** (the `FanOutNotification` job; a dedup hit publishes nothing): `notification` to the recipient's `user` (with notifications in scope) and `user:notification` streams.
- **Block**: the blocker's and the blocked account's policy epochs rise (see below). Also, for the blocker's home and list streams, `delete` for the retraction set. The set is read before the commit (a block unfollows, so the feed's authors change): statuses by the target, reblog wrappers by the target, reblogs of the target's statuses, and statuses mentioning the target, from the viewer's home authors and list members. It is capped at 200 statuses (D15).
- **Mute**: the muter's epoch rises, and the same retraction runs for the muter.
- **Unfollow, remove follower, domain block**: the affected viewer's epoch rises. No `delete` is sent, as upstream does not send one for an unfollow or a domain block.
- **Token revoked** (`POST /oauth/revoke`): every connection of that token closes (the SSE body ends; the WebSocket receives a close frame). Upstream's `kill`.
- **Filter create, update or delete**: no event. Upstream's `filters_changed` has no payload, so its streaming server never transmits it. `filtered` is computed at delivery, so a filter change applies to every later frame.
- **Not yet published** (no source in birchtree yet): account suspension or disablement (`kill`, Phase 7.3; D24); conversation and announcement events (Phase 7); `notifications_merged` (upstream sends it when a notification request is merged; birchtree has no request filter, so it is never sent; D20).

## Ordering, liveness and the no-leak rule

- Frames for one subscription come in the order their events committed. The fan-out is a single thread.
- Delivery is at most once. A frame is dropped if the connection is closed, if its queue is full, or if the policy epoch below moved past it.
- Gates read durable state when the fan-out handles the event. A status deleted before its create is handled is not announced.
- **Policy epochs.** Each connection carries an epoch. A block, mute, unfollow, remove-follower or domain block raises the epoch of the affected viewers' connections. A frame queued under an earlier epoch is dropped at the transport. A `Retract` is published after the epoch change, so its frames carry the new epoch.
- **Guarantee.** After a block, mute, unfollow or domain block returns, no event for a status committed after that return reaches the affected viewer. Events for statuses committed before it may still be in flight; any still queued when the change is processed are dropped.
- **Overflow.** If the event queue is full, the hub does not drop the event silently. It closes every stream, and clients reconnect and reread state (fail closed).

## Bounds, drop and disconnect policy

| Bound | Default | On breach |
|---|---|---|
| Connections, total | 512 | `503` at open |
| Connections, per token | 16 | `429` at open |
| Subscriptions per WebSocket | 16 | error frame, `429`; the socket stays open |
| Frames queued per connection | 256 | the connection closes |
| Bytes queued per connection | 1 MiB | the connection closes |
| Event queue (publish to fan-out) | 4096 events | every stream closes (overflow) |
| Stall: frames queued and no transport progress | 30 s | the connection closes |
| SSE heartbeat | 15 s (`:thump`) | not applicable |
| WebSocket ping | 30 s; a socket that did not pong since the last ping closes | the connection closes |
| Retraction per block or mute | 200 statuses | the rest stay in the client until it reconnects (D15) |
| List members read for a list stream | 500 | birchtree's `LIST_MEMBER_CAP` (`application/timelines.rs`); upstream reads every member |
| Home follows read | 1000 | as D10a |

A connection closed for any of these reasons ends its SSE body or closes its WebSocket. Its subscriptions are removed, and so is its registry entry, even when the client vanished mid-response (the session is released when the transport is dropped).

Limits are configurable (`streaming::Limits`); the defaults above are the production profile.

## Deviations and gaps

The register (`docs/compatibility-profile.md`) carries each row. In summary:

- D14: the caps and queue bounds above (upstream has none; Redis buffers instead).
- D15: the retraction cap.
- D16: a streaming path on the configured streaming host's own origin is served, not `404`.
- D17: `Vary: Authorization` on streaming responses (upstream's Node service sends none; no client reads it).
- D18: the `direct` stream is silent until Phase 7.
- D19: the `public:remote` streams are silent until Phase 6.
- D20: `notifications_merged` is never sent (no notification-request filter).
- D21: no `chosen_languages` filter on public and hashtag streams (the setting does not exist in birchtree; upstream applies no filter when it is unset).
- D22: home and streams ignore blocks and mutes on a status's mentions. Upstream's `FeedManager#filter_from_home` applies them. This is a Phase 4 policy gap, shared with the REST home timeline; escalated.
- D23: the home gate does not apply a follow's language preference (`Follow.languages`); upstream does. Also a Phase 4 gap, shared with REST.
- D24: no `kill` on account suspension or disablement (no suspension exists before Phase 7.3).
- D25: hashtag normalization is the REST normalizer, not upstream's `normalizeHashtag`; the streams and the REST tag timeline agree with each other, not with upstream, for accented or compatibility-form tags.
- D26: notifications from blocked, muted or domain-blocked accounts are created and streamed; upstream drops them.

## Verification

- `tests/phase5_streaming.rs` (31 tests on real sockets through the real router): refusals and their texts; SSE headers; health; the redirect; each stream family; block, mute and unfollow; edit, delete and mention edits; notifications; revocation; the WebSocket subscribe protocol and error frames; the caps and heartbeat; the queue bound; the policy epoch; overflow; the stall sweep; and the concurrent follow, unfollow, block and delete race with the no-leak check.
- Unit tests: `src/streaming/channels.rs` (names, channel ids, parameters), `src/streaming/auth.rs` (token precedence, scopes).
- `scripts/smoke-phase5.sh` (live binary, `curl -N` and a stdlib WebSocket client): `SMOKE_PHASE5_OK`.
- Mutation checks (2026-10-17, verification log §3): the epoch drop, the retraction and the mention gate each make their tests fail when disabled.
- Not run: the unmodified-client parity suite against a live Mastodon v4.7.3 (`birchtree.md` §8 item 11). See the verification log and the escalation list.
