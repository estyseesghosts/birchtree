# Federation

Scope: `src/activitypub/*`, `src/federation/*`, vendored `vendor/activitypub_federation` (`activitypub-federation 0.6.6` + 4-item patch), `docs/federation-contract.md`, `docs/federation_repair.md`, `ACTIVITYPUB-FEDERATION-RUST.md`. Mastodon behavior cited is v4.7.3 (`8ff11c2b`). Misskey behavior cited is the `fedifedifedi/misskey` checkout.

## 1. How birchtree federates

### 1.1 Crate vs hand-rolled split

`Cargo.toml:44-45,59-61`: `activitypub-federation =0.6.6` with `[patch.crates-io]` → `vendor/activitypub_federation`.

- Crate owns: HTTP-signature create/verify + `Digest` check, `receive_activity` dispatch, `ActivityHandler{id,actor,verify,receive}`, `ObjectId::dereference`, `SendActivityTask::prepare/sign_and_send` (shared-inbox dedup, local-domain exclusion), fetch wire rules.
- birchtree owns: `SafeHttpConfig` / `PolicyVerifier` / `PolicyResolver` (`src/federation/net.rs`, `policy.rs`), wire DTOs (`src/activitypub/wire.rs`, `objects.rs`, `actor.rs`), the `Activity` enum (`src/activitypub/activities.rs:44-59`), routes / NodeInfo / collections / authorized-fetch gate (`src/activitypub/http.rs`), durable `FederationEvent -> DeliverActivity` fan-out and retry/budget/cooldown/quarantine (`src/federation/events.rs`, `delivery.rs`), thread completion (`threads.rs`), remote media/emoji (`media.rs`), tombstones (`tombstone.rs`), dedup (`dedup.rs`, table `ACTIVITY_DEDUP`, 7-day TTL).

REST and ActivityPub call the same `application::federation{,_inbound,_ports}` use cases.

The vendored patch is exactly 4 items: `MAX_INBOX_BODY_SIZE = 1 MiB` (upstream crate 200 KiB → Mastodon 1 MiB, D27; remote docs keep 200 KiB); `verify_signature` made `pub` for authorized-fetch GET checks; PEM trim + single trailing newline (Akkoma blank-line key 500 → 401); re-fetch actor once on signature failure unless fetched < 60s ago (`KEY_REFRESH_COOLDOWN_SECS = 60`).

### 1.2 Discovery / actor / keys / inboxes / collections

| Route | Handler |
|---|---|
| `GET /.well-known/webfinger?resource=acct:name@domain` | `webfinger` |
| `GET /.well-known/nodeinfo` | `nodeinfo_links` |
| `GET /nodeinfo/2.0` | `nodeinfo` |
| `GET /actor` | `instance_actor` |
| `GET /users/:username` | `actor` |
| `GET /users/:username/statuses/:id` | `note` |
| `GET /users/:username/quote_authorizations/:id` | `quote_authorization` |
| `GET /users/:username/followers`, `/following` | `federation_collection` |
| `GET /users/:username/collections/featured` | `featured_tags` |
| `GET /users/:username/outbox` | `outbox` |
| `POST /inbox` (shared) | `shared_inbox` |
| `POST /users/:username/inbox` (404 unless local) | `personal_inbox` |

WebFinger returns `application/jrd+json`; suspended accounts answer `410 Gone`. NodeInfo 2.0 serves measured `usage{users.total/activeMonth/activeHalfyear, localPosts}`, `openRegistrations`, `metadata{nodeName,nodeDescription}` (never fabricated, D31; `software.name` is `birchtree`, D48). Actor is `Person{id,preferredUsername,name?,summary?,inbox,endpoints{sharedInbox},publicKey,followers,following,outbox,alsoKnownAs:None,featuredTags,icon?,image?}`; `verify` checks id host + `publicKey.owner == id`; remote avatar/header attachments are intentionally not ingested. Keys: RSA-2048 PKCS#8/SPKI, `keyId = {actor}#main-key`; `/actor` application actor id `0`; per-account generation is single-flight, winner-wins. Inboxes dispatch to `receive_activity`; unknown types answer `202 ACCEPTED` (accepted-and-dropped, D32-narrow). Collections: followers/following base + `?page=N` (12/page, cap 1000, `hide_collections` → empty items + real total, D52); outbox base + `?page=true&min_id/max_id` cursor pages (20/page, public only); `replies` on every Note; `featuredTags`; `quote_authorizations/:id`.

### 1.3 HTTP signatures

Outbound: legacy draft-Cavage `rsa-sha256` via `aws-lc-rs`, `Config::mastodon_compat`, `EXPIRES_AFTER = 1h`, headers `(request-target) content-type date digest host`, `algorithm="hs2019"`. Inbound: `verify_body_hash` (`SHA-256=Base64(sha256(body))`, missing → 401) then parse then `verify_signature` + one forced-refetch retry on stale key. **Not supported:** Ed25519 / FEP-521a, RFC-9421 `rsa-v1_5-sha256` / `ed25519`. Outbound is legacy-Cavage-only; RFC-9421-only senders 401.

### 1.4 Safe HTTP client

No proxy routing; `redirect(Policy::none)` (crate follows one redirect itself and re-validates). Resolver refuses a name when **any** resolved address is blocked (rebinding-safe). Connect 5s / request 10s, `User-Agent: birchtree/<version>`, optional extra roots. Policy: https-only unless lab-allowed, no creds, IP literals judged directly; blocks `0/8,10/8,127/8,169.254/16,172.16/12,192.168/16,192.0.0/24,192.0.2/24,100.64/10,198.18/15,198.51.100/24,203.0.113/24,≥224`, `::/128,::1,fe80::/10,fc00::/7,2001:db8::/32,100::/64,ff00::/8`, mapped `::ffff:*`, NAT64 `64:ff9b::/96`, 6to4, plus `--federation-allow-private` allowlist. Delivery POSTs treat any 4xx as retryable under budget (D28); fetches pass 404 → terminal, 410 → deleted.

## 2. Ingress and egress paths

`POST /inbox` or `POST /users/:username/inbox`: bound body (1 MiB) → authenticate (digest + signature + one stale-key refetch; failure 401; unknown type → 202 at route) → validate (strict types, domain-match id/actor/attributedTo/object, `contentMap` canonicalization, vote-vs-status split, liberal poll `closed`, length caps, 200 KiB remote-doc cap) → dedup (`record()` only `Applied`/`Duplicate`, never `Ignored`, D33; same-host Delete-beats-Create tombstones) → apply exactly one application command (follow/accept/reject/undo, ingest/update/delete/vote/sync, like/unlike, announce/unannounce, block/unblock, flag, quote_*) with follow-up jobs (`ResolveParent`, `FetchRemoteMedia/Emoji`, `CrawlReplies`, `FanOutNotification`, `RelayForward`) in the same txn → 200/202. Failures never roll back committed local actions. Acks: bad sig/digest 401; policy/domain/forgery 403; unknown account/status or fetch 404; suspended/tombstone 410; parse/invalid 422 (unsupported coerced to 202); conflict 409; remote fetch/parse/size/timeout 502; else 500. Authorized-fetch reads (`note`, `quote_authorization`, collections, outbox) require a valid signature when `authorized_fetch` is on, else 401.

Egress: REST/application commits `FederationEvent` in the same redb txn as the state change; no outbound HTTP inside the local txn. `events::run` builds fixed JSON then one `DeliverActivity{sender,inbox,activity_uri,body}` per **distinct** inbox (shared-inbox dedup). Only public create/update/delete/announce/question_update also fan out to configured relays (`RelayForward`). Per-destination: `COOLDOWN_BASE 60s` doubling cap 1h; cooldown → completed-now + deferred copy (park does not consume budget); 2xx resets host; only retryable failures charge the budget (`FAILURE_BUDGET 10`); permanent (refused URL/missing key/unparseable) quarantines without cooling; max 25 deferrals → quarantine (tail ≈ 1 day, D49, vs upstream `DeliveryWorker retry:16`). One failing host never blocks others. `profile_update` sends immediately to remote followers only; poll tallies fan out as `question_update` unless hidden.

## 3. Activity table

Untagged enum, order matters. Outbound IDs are actor-local.

| Activity | Inbound | Outbound | Notes |
|---|---|---|---|
| Follow | object must be local, deref both, `receive_follow` + synthesize `Accept` | `{follower}/follows/{id}` + pending row one txn | 14/14 follow-family interop logged |
| Accept | `directed_at == actor` else refused; `receive_accept` or `quote_accepted + record` (needs `result`) | Follow-Accept synthesized inbound; quote-Accept `{author}#accepts/quote_requests/{id}`, `result = approval_uri` | `AcceptObject: Follow \| QuoteRequest \| Id` |
| Reject | same gate | `{author}#rejects/quote_requests/{id}` | — |
| Undo{Follow,Like,Announce,Block} | embedded actor == outer; undone Follow targets local | `{follower}/undo/{id}`, `{like_id}/undo`, `{boost_uri}/undo`, `{block_id}/undo` | ignored never recorded → redelivery re-evaluates |
| Create{Note,Question,Id} | vote-shaped Note tallies instead of ingesting; else tombstone-checked ingest + parent scheduling | `{uri}/activity`, `Question` when poll; vote `Create{Note{name,inReplyTo,to:owner}}` | embedded `replies` not crawled inline; only parent hop scheduled |
| Update{Note,Person,Question} | Note parse+verify+update; Person forced refresh; Question sync-or-ingest | `{uri}#updates/{edited_at}`, `{uri}#updates/{now}` per tally (hidden-totals never sent), profile `{actor}#updates/{now}` | unknown type → 202 |
| Delete | `object = id \| {id}`; approval-URI → revoke; `== actor` → delete account; else delete note + tombstone | `{uri}#delete` with `Tombstone/formerType:Note`; actor `{actor}#delete` with `Tombstone/formerType:Person` | Delete-beats-Create both directions (D38 batch 50/job) |
| Like / Announce / Block / Flag | fetch-unknown Announce target first; Block target must deref + be local | `{actor}#likes/{target}`, `id = boost_uri` (host == booster), `{actor}#blocks/{target}`, `{actor}#reports/{id}` with `object = [target, ...statuses]` | domain blocks never federated (parity) |
| QuoteRequest (+Accept/Reject/Delete(QuoteAuthorization)) | instrument ingested via tombstone check; `Accept(result?)`/`Reject`; `Delete(QuoteAuthorization)` → revoke | `QuoteRequest id = quote_uri`, `object = quoted`, `instrument = quoting Note`; accept/reject/revoke | FEP-044f + `quote`/`quoteAuthorization` inbound; `quoteUri`/`_misskey_quote` aliases parsed; **no** `quoteUri` duplicate, `RE:` fallback, or `interactionPolicy` on emit |
| Move | `object == actor`, `target != actor`, target derefs, old remote; migrates **local** followers only (cap 10k), blocked/self skipped, locked → pending; `alsoKnownAs` **not** checked (D32a) | no Move sender (local moves out of profile) | hand-rolled `MoveType` (crate has none) |

Tail (landed): relays + inbox-forward (only Announce/Create); remote counters hints-only; `QuoteReverify` (hop 8, budget 100); collection updates; live WebFinger; remote oEmbed; `SyncFollowers`. Still phased: `Add`/`Remove` (fanout + notifs only), `FeatureRequest/Authorization` (route, no handler), `DeleteActor` serve outbound-only, Question votes inbound-only as `Create(Note)`.

## 4. Thread completion

Ingest-time: inbound reply with unknown parent + valid URI → durable `ResolveParent{child,parent_uri,hop=0}`. Each job: skip if resolved; link if local (chain next hop); else signed dereference via instance actor; hop cap 40 (proposal); terminal (not-found/blocked/unsafe/budget) stays unresolved, never promoted; transient retries. Withheld from home/list + ordinary reads until linked. Read-time descendant: authenticated `GET context` on remote + distributable + age ≥ 5m + no refresh in 15m → `200` + `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]` + durable `CrawlReplies` (BFS over `replies.first`, 500/collection, 500 pages, 1000 total, signed + safe client). Read-time ancestor (decided deviation D5): same `context` walks up 40; first unresolved parent gets `ResolveParent(hop=0)`; shared refresh id; anonymous never enqueues. Missing vs upstream: no inline 5-reply fetch on Create; no per-reply worker queue (single `CrawlReplies` per status).

## 5. Remote media + emoji

Outbound emit: `Document{type/mediaType/url/name + blurhash/width/height?}`. Inbound: metadata-only ingest → `FetchRemoteMedia` on first request (warming default off, relay-excluded) → instance-signed GET per hop (max 5 redirects) → 5xx/429/408 retry else quarantine → 40 MiB streaming cap → `tmp/{rand}.part → publish_tmp` → sniff+validate → per-row `remote/<media_id>` + ledger reserve/commit → `ProcessMedia` → `Ready`. Pending rows serve `url: null` (Mastodon `not_processed?` parity); `remote_url` stays `Null` in the DTO (open item). Emoji: max 50/object, 256 KiB, PNG/GIF/WebP, namespaced `{shortcode}@{domain}`, never picker/listed.

## 6. Interop

### birchtree ↔ birchtree

Works both directions (lab + `tests/phase6_*`): Follow/Accept/Reject/Undo, Create/Update/Delete Note/Question, votes, Like/Announce/Block/Flag, QuoteRequest-Accept-Reject-Revoke, tombstone races, 403-domain fix, per-host isolation, polls, profile/delete fan-out, question updates, outbox/followers/following paging, tombstone 410s.

### birchtree ↔ Mastodon (v4.7.3 expected)

Expected to work: WebFinger `acct:` → canonical actor URI; NodeInfo 2.0; `Person` with `publicKey`, inbox/sharedInbox, followers/following/outbox, featuredTags, icon/image; signed Follow/Create/Update/Delete/Like/Announce/Undo/Flag/QuoteRequest; edits, deletes, interaction changes, poll votes/tally sync, profile `Update`, account-delete `Delete`, thread completion against authorized-fetch remotes (signed fetches only), `context` paging + `Mastodon-Async-Refresh`, tombstone 410s, activity-ID dedup idempotence downstream. Known deltas: RSA-only (no RFC-9421/Ed25519 — modern-only peers 401); quotes emit only `quote` + `quoteAuthorization` (no `quoteUri` dup, `RE:` fallback, or `interactionPolicy` — Mastodon policy UI sees default); poll fan-out narrower (followers + mentions + parent vs mentions + rebloggers + voters); no collection-sync digest/header; no `Follow` option carriage (bare — parity); single `CrawlReplies` vs per-reply queue; `remote_url` null; display-name emoji local-only; remote avatar out of profile; `likes`/`shares`/`followers_synchronizations`/`feature_authorizations`/`featured_collections` subset only; domain blocks local-only both sides but no sever cascade.

### birchtree ↔ Misskey

Misskey emits `_misskey_quote` + `quoteUrl` together (both optional; context aliases; reads `_misskey_quote ?? quoteUrl`); no FEP-044f consent flow — a quote is a bare URL rendered inline. birchtree parses `quote` + `quoteUri` + `_misskey_quote` (not Misskey's newer `quoteUrl` key — but Misskey co-emits `_misskey_quote`, so the reference is still captured) and enters its `QuoteRequest` state machine; Misskey renders birchtree `quote` links but ignores `quoteAuthorization` and never sends Accept/Reject. Expect: Misskey → birchtree quotes ingest (via `_misskey_quote`) and stay pending/manual unless approved locally; birchtree → Misskey quotes arrive as plain quote links (authorization unenforced on the Misskey side). Polls (`Question`, `oneOf`/`anyOf`, `endTime`/`closed`, `votersCount`) and draft-Cavage RSA signatures interoperate; locked Misskey instances need the signed-fetch path birchtree already uses.

### Generic ActivityPub software

Any compliant server can expect: stable HTTPS actor URLs (`/users/{username}`, `/actor` instance actor), `Person` with `publicKey`, inbox + sharedInbox, `followers`/`following`/`outbox` (`OrderedCollection` + pages), `Note`/`Question` with `to`/`cc`, Mention/Hashtag/Emoji tags, `Document` attachments, `replies` (`{id,type:Collection,totalItems,first}`), Mastodon-compat emit fields (`contentMap`, `sensitive`/`summary`, blurhash, dimensions, `quote`/`quoteAuthorization`), `202 ACCEPTED` accept-and-drop for out-of-profile kinds, Delete-beats-Create (tombstoned URIs stay gone, served as `Tombstone` 410), authorized fetch (401 without valid signature when enabled), at-least-once per-destination delivery idempotent under activity-ID dedup.

## 7. What is NOT supported

Ed25519/FEP-521a; RFC-9421 verify/emit; Move sender (`also_knownAs` never checked); `Add`/`Remove` send/receive; `FeatureRequest/Authorization` handlers; `DeleteActor` serve inbound; Question votes outbound; quote `quoteUri` dup / `RE:` fallback / `interactionPolicy` on emit (inbound reads `quote`, `quoteUri`, `_misskey_quote` — not newer `quoteUrl`); `remote_url` rendering; remote avatar caching; `card`; collection-sync digest/header; `Follow` option carriage; sever cascades; `can_deliver_to_inbox`; per-reply fetch queue; inline 5-reply fetch; profile debounce/reporters expansion.
