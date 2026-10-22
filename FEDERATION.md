# Federation

**FEP-67ff:** this file is birchtree's `FEDERATION.md` record for
interoperability implementers.

- **Software:** birchtree — a single-process, Mastodon-API-compatible
  fediverse server (Rust, embedded `redb`, local-disk media).
- **Audited revision:** `master` at `8712ae8`
  (`Adv2 groundwork: disabled-mode refusal, …`, 2026-10-22).
  The working tree was dirty and one commit ahead of `origin/master`
  (`e82343e`), so freshness against upstream could not be fully established.
  All claims below describe that revision only.
- **Compatibility target:** [Mastodon **v4.7.3**](https://github.com/mastodon/mastodon/tree/v4.7.3)
  (API v7, pinned commit `8ff11c2b`).
- **Test status:** interop is verified in a private Docker-bridge lab against
  real Mastodon 4.7.3 plus a Misskey-family peer (`pinetree`) — never against
  the live public fediverse. Lab-only switches (`--federation-allow-http`,
  `--federation-allow-private`, `--federation-extra-ca`) are off by default
  and must never be used in production.

## Federation overview

birchtree federates as a **Mastodon-compatible ActivityPub server-to-server
(S2S) peer**. It does not implement ActivityPub client-to-server (C2S);
clients use the Mastodon REST + streaming API instead.

Entities it federates:

- **Actors:** local users are `Person` actors (`/users/:username`); a reserved
  instance actor (`Application`, `/actor`) signs thread-completion fetches.
- **Objects:** `Note` (statuses), `Question` (polls), `Tombstone`
  (deletions), `QuoteAuthorization` (accepted quotes), `Document` (media
  attachments), `Emoji` tags (inbound only), `Hashtag`/`Mention` tags.
- **Collections:** `followers`/`following`/`outbox` per actor,
  `featuredTags` (hashtags only), `replies` linkage for thread completion.

Protocol work is delegated to the
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
crate (pinned to `=0.6.6`, vendored with a 1 MB inbox body-cap patch; see
`vendor/README.md`). birchtree's own `activitypub/` (wire types, actor,
inbox handlers) and `federation/` (resolution, delivery, retry, dedup,
remote media, thread completion) layers own policy, persistence, and
Mastodon semantics. The crate's checks are necessary but not sufficient:
application handlers enforce authorship, authorization, and visibility.

Inbound input is untrusted: signatures are verified, URLs pass SSRF policy,
and payloads pass size, timeout, redirect, and content-type limits before
any state change commits. One `redb` transaction commits state mutation +
indexes + idempotency records + durable delivery jobs atomically; all
network I/O happens after commit via durable workers.

## Supported federation protocols and standards

| Protocol | Role | Spec |
|---|---|---|
| ActivityPub, server-to-server | Core federation protocol | <https://www.w3.org/TR/activitypub/> |
| WebFinger (`acct:` resolution) | Actor discovery, inbound and outbound | <https://datatracker.ietf.org/doc/html/rfc7033> |
| HTTP Signatures, draft Cavage `rsa-sha256` | Request signing and verification (RSA-2048 keys) | <https://datatracker.ietf.org/doc/html/draft-cavage-http-signatures-12> |
| NodeInfo 2.0 | Instance capability metadata | <https://nodeinfo.diaspora.software/> |

Mastodon-specific wire behavior follows
[Mastodon v4.7.3 `FEDERATION.md`](https://github.com/mastodon/mastodon/blob/v4.7.3/FEDERATION.md),
the [ActivityPub behavior notes](https://docs.joinmastodon.org/spec/activitypub/),
and the [security notes](https://docs.joinmastodon.org/spec/security/),
with the deviations registered in `docs/compatibility-profile.md`.

Explicitly **not** implemented: ActivityPub C2S, RFC 9421 HTTP Message
Signatures (inbound RFC-9421-shaped requests are refused with `401` and no
state change), Ed25519 keys / FEP-521a, Linked Data / Data Integrity Proofs,
FEP-ef61 portable objects, MRF-style relay subscription protocols.

## Supported FEPs

| FEP | Status | Notes |
|---|---|---|
| [FEP-67ff](https://w3id.org/fep/67ff): `FEDERATION.md` | Supported | This document. |
| [FEP-044f](https://w3id.org/fep/044f): Consent-respecting quote posts | **Partial** | `QuoteRequest` → `Accept`/`Reject` (with `result` approval URI) → `Delete(QuoteAuthorization)` revoke flow works both directions (see below). Deltas: emitted quotes carry `quote` + `quoteAuthorization` only — no `interactionPolicy`, no `quoteUri` duplicate, no `RE:` fallback. Ingest accepts `quote`, `quoteUri`, and `_misskey_quote` aliases but not the newer `quoteUrl` key (works only while Misskey dual-emits). Quote re-verification revokes only on 404/410 or locally-gone status (`QuoteReverify`, max 8 hops, budget 100). |
| [FEP-f1d5](https://w3id.org/fep/f1d5): NodeInfo in Fediverse Software | Supported | `/.well-known/nodeinfo` link (`rel` `http://nodeinfo.diaspora.software/ns/schema/2.0`) + `/nodeinfo/2.0` with real measured counts (local users, 4w/24w active sign-ins, local posts; scan capped, failures reported as `0`). |
| [FEP-6757](https://w3id.org/fep/6757): Content license metadata | **Experimental** | Minimal integer-license profile only, policy-gated (`--license-policy=off\|unenforced\|enforced`, default `off`); invalid values are dropped to null at ingest. |

Not supported as FEPs (do not expect them): FEP-521a (no Multikey —
RSA `publicKey` only), FEP-8fcf (remote follower/following totals are
ingested as display **hints only**, never reconciled; no sync protocol),
FEP-ae0c relays (outbound forwarding to operator-configured relays only, no
relay `Follow`/`Add`/`Remove` membership handling), FEP-e232, FEP-ef61,
FEP-f228, FEP-9091, FEP-c0e0. No `FEP-*` string other than 044f/521a/6757
appears in `src/`; absence was verified by search, not assumed.

## Actor discovery and addressing

### WebFinger — `GET /.well-known/webfinger?resource=acct:user@domain`

- Live lookup against the account table (not static): unknown name → `404`,
  suspended account → `410 Gone`.
- `acct:` user and domain must both match this instance; otherwise refused.
- Response is `application/jrd+json` with `subject`, a `profile-page`
  (`text/html`) link and a `self` (`application/activity+json`) link to the
  actor URI, plus empty `aliases`/`properties`.
- Outbound: remote `user@domain` handles resolve live over WebFinger (HTTPS
  unless lab `--federation-allow-http`), taking the first
  `application/*` link target and dereferencing it; URL validity is
  re-checked if the fetch redirects. Authenticated REST account
  search/lookup also resolves unknown remote handles live.

### Actor documents

- **Users:** `GET /users/:username` serves `application/activity+json`
  `Person` with `id`, `preferredUsername`, `name`/`summary` (when set),
  `inbox`, `endpoints.sharedInbox` (`{public-url}/inbox` for locals),
  `publicKey` (`{actor}#main-key`, RSA SPKI PEM), `manuallyApprovesFollowers`
  (locked accounts), `followers`/`following`/`outbox` (locals only),
  `alsoKnownAs: null` (never populated today — see the `Move` row under
   [ActivityPub interoperability](#activitypub-interoperability)),
  `featuredTags` (locals only), and `icon`/`image` only while an avatar/header
  blob is staged.
- **Instance actor:** `GET /actor` serves an `Application` document
  (`preferredUsername` = host, inbox, `publicKey`) used to sign
  thread-completion fetches. No `sharedInbox`/`outbox`/`followers`.
- **Keys:** RSA-2048 only, generated at account/instance-actor creation
  (`PKCS#8` private sealed in the OS keyring store, `SPKI` public served).
  No Ed25519 key is generated, published, or verified. Remote keys are cached
  and refetched when older than 24h; a signature failure against a stale key
  triggers one forced refetch and re-verification.
- **Object route:** `GET /users/:username/statuses/:id` serves `Note`
  (or `Question` for polls); deleted → `410` with a `Tombstone` document;
  non-public (followers-only/direct) → `404`. Tombstones keep the
  `STATUS_BY_URI` mapping so redelivered `Create`s stay duplicates.
- **Quote authorization:** `GET /users/:username/quote_authorizations/:id`
  serves the `QuoteAuthorization` of an accepted quote.
- **Collections:** `followers`/`following` return `OrderedCollection` with
  `totalItems` and `?page=` paging (12 per page, up to 1000 pages);
  `outbox` is served; `collections/featured` serves `featuredTags` as an
  `OrderedCollection` of `Hashtag`s. The `hide_collections` operator setting
  empties collection members while keeping `totalItems`.

### Signed fetch / authorized fetch

- Outbound dereferences are signed with the instance actor's key by default.
  Media fetches stay signed only while same-origin; a cross-origin redirect
  is fetched unsigned (logged as `signature_stripped`).
- `--federation-authorized-fetch` (default off) requires a valid HTTP
  Signature on `note`, `quote_authorization`, `followers`, `following`,
  `featured_tags`, and `outbox` reads; unsigned requests get `401`.
  `actor`, `webfinger`, `nodeinfo`, and `instance_actor` stay public.
  Inboxes always require valid signatures regardless of this flag.

## ActivityPub interoperability

Mastodon compatibility target for all of the below is v4.7.3; behavior was
checked against the pinned tag, not `main`.

### Inbound activities

Inbox: `POST /inbox` (shared) and `POST /users/:username/inbox`
(personal; unknown local user → `404`). Success and
accepted-but-unsupported both return `202`; signature/digest failures →
`401`; unparseable activities → `422`; refused (e.g. wrong-domain) → `403`;
fetch failures → `502`-class. Malformed or out-of-profile activity types
fail parsing and never change state.

| Activity | Inbound support | Behavior |
|---|---|---|
| `Follow` | Yes | Accepted only for local target users; blocked pair → ignored. Auto-builds deterministic `Accept` (`{target}#accepts/{follow-id}`) and delivers it; locked accounts go pending and the `Accept` is dropped. Redelivery → `Duplicate`. |
| `Accept` (follow / quote) | Yes | Must be directed at the local actor. Follow: applies the follow grant. Quote: requires `result` approval URI; unknown/non-pending quote → ignored, already-accepted → duplicate. |
| `Reject` (follow / quote) | Yes | Mirrors `Accept`. |
| `Undo(Follow\|Like\|Announce\|Block)` | Yes (closed set) | Embedded actor must equal the outer actor; `Undo(Follow)` additionally requires a local object. Unknown-target `Undo(Like/Announce)` → duplicate (delete-wins); unknown-target `Undo(Follow/Block)` → ignored and **not** recorded (see D33 below). Other `Undo` payloads fail parsing. |
| `Create(Note\|Question\|Id)` | Yes | `Note`: authorship enforced (`attributedTo` == actor, same host). Vote-shaped Notes (`name` set, empty content, `inReplyTo` set) tally against known local polls or ingest as statuses. `Question`: must have expiry + 2–10 named options or it ingests poll-less (never `422`). Bare-ID `Create` dereferences the object. Tombstoned URIs are dropped as duplicates. Unknown parents schedule one-hop `ResolveParent` jobs (cap 40 hops). |
| `Update(Note\|Person\|Question)` | Yes | Author-equality enforced; unknown target → ignored. `Person` refreshes profile + key (always applied). `Question` on a known status syncs tallies/options only if author/poll/options match, else ignored; on an unknown status it ingests as a `Create` (out-of-order delivery). Other `Update` object types → accepted-and-dropped (`202`). |
| `Delete` | Yes | Three branches: approval-URI → quote revocation (quoted author only); object == actor → account deletion; else status deletion (wrong author → ignored). Unknown same-host object URIs file a tombstone (cross-host `Delete`s never tombstone); `Delete` beats a racing `Create` on every ingest and dereference path. |
| `Like` / `Undo(Like)` | Yes | Unknown/invisible target → ignored (`Like`) / duplicate (`Undo`). |
| `Announce` / `Undo(Announce)` | Yes | Boosted object is fetched first (fetch failure still proceeds to apply). Visibility derived from `to`/`cc` with exact `{actor}/followers` match. Unknown/invisible → ignored / duplicate respectively. |
| `Block` / `Undo(Block)` | Yes | Remote-target `Block` → ignored; local target → applied. |
| `Flag` | Yes | Collects known statuses + local accounts (including status owners); remote-only → ignored. Otherwise files one local report per local target (`comment` truncated to 5000 chars, `forwarded: false`). Inbound `Flag` never re-federates. |
| `QuoteRequest` | Yes | Inline `Note`/`Question` instruments are author-verified and ingested (tombstoned → ignored); ID instruments are dereferenced. Non-local quoted target → ignored; existing accepted → duplicate. Otherwise resolved against local quote policy (`public`/`followers`/`nobody`), setting `Pending` → `Accepted`/`Rejected` and enqueueing the outbound `Accept`/`Reject`. |
| `Move` | **Partial** | Verified: `object` == actor, `target` != actor; old account must be remote, target must dereference. Migrates **local followers only** (cap 10,000; skips remote/self/blocked/already-following; locked successor → pending), records successor, enqueues follower-count sync. **Not verified:** `alsoKnownAs` link-back is never checked (served actor documents always emit `alsoKnownAs: null`). Forged `Move`s fail only at target dereference. |
| `Add`/`Remove`, `Article`/`Video`/`Image`/`Event` creates/updates, standalone `Emoji`, `Invite`/`Join`/`Leave`, `View`, `DeleteActor` type, `FeatureRequest`/`FeatureAuthorization` | No | Fail `Activity` parsing → `422`, never applied. Status pins/endorsements have no federated sync. |

**Deduplication (D33):** applied activities and duplicates are recorded by
canonical activity URI (7-day TTL); no-effect (`Ignored`) activities are
deliberately **not** recorded, so redelivery is re-evaluated (this matches
Mastodon's `Undo`-ID reuse).

**Recipient/visibility gating:** visibility is *derived* from `to`/`cc`
(public / unlisted / followers-only / direct); no `to`/`cc` recipient gate
was found — any signature-valid, author-correct `Note` is ingested even if
unaddressed. Mentions resolve only to already-known accounts; hashtags are
normalized (NFKC + lowercase); quotes link only to known local non-self
statuses.

### Outbound activities

Local REST actions enqueue a `FederationEvent` (or direct `DeliverActivity`
for follows) in the **same** `redb` transaction as the state change; workers
fan out per-destination inbox POSTs afterwards. Payload bytes are fixed at
enqueue time, so retries send identical bodies. Follows/unfollows, likes,
blocks, votes, and quote decisions deliver to the target inbox only (shared
inbox preferred); public content fans out to deduped follower inboxes.

| Local action | Activity sent | Recipients |
|---|---|---|
| Post / reply (poll → `Question`) | `Create{Note\|Question}` | Mentions + reply-parent author + followers (skipped for direct), per-inbox dedup |
| Edit (+ accepted-quoter + live-reblogger updates) | `Update{Note}` (`{uri}#updates/{edited_at}`) | Same audience as create |
| Delete status (single or 50-per-job account-deletion batches) | `Delete{Tombstone, formerType: Note}` (`{uri}#delete`, `to: [Public]`) | Same audience as create |
| Follow remote | `Follow` (`{actor}/follows/{id}`) | Target inbox only |
| Accept / Reject inbound follow | `Accept`/`Reject{Follow}` | Follower inbox (dropped when the target is locked/pending) |
| Unfollow | `Undo{Follow}` | Target inbox only |
| Like / Unlike | `Like` (`{actor}#likes/{status}`) / `Undo{Like}` | Boosted/liked owner's inbox only |
| Boost / Unboost (incl. reblog-delete) | `Announce` / `Undo{Announce}` (`{boost}/undo`) | Boost: boosted author + followers; **Undo: boosted author only** — followers are not re-notified (known gap) |
| Block / Unblock | `Block` (`{actor}#blocks/{id}`) / `Undo{Block}` | Target inbox only |
| Poll vote on remote poll | N× `Create{Note{name: choice, inReplyTo: poll}}` to the poll owner | Poll owner inbox only (local-poll votes send nothing) |
| Poll tally change / expiry | `Update{Question}` (skipped when `hide_totals`) | Status audience |
| Profile edit (any write) | `Update{full actor document}` (`{actor}#updates/{now}`) | Remote followers only; immediate, no 5s debounce, no reporter/mention expansion (intended) |
| Account delete | `Delete{Tombstone, formerType: Person}` (`{actor}#delete`, `to: [Public]`) | Followers only (no relays) |
| Report with forwarding | `Flag{object: [target + existing statuses], content}` (`{actor}#reports/{id}`) | Target inbox only |
| Quote local→remote initiated | `QuoteRequest{object, instrument: quoting Note}` | Quoted author's inbox only |
| Quote accept / reject | `Accept`/`Reject{QuoteRequest, result: approval-uri}` | Quoting author's inbox only |
| Quote revoke (+ `QuoteReverify` worker path) | `Delete{QuoteAuthorization}` (`{approval}#delete`, `to: [Public]`) | Quoting author's inbox only |
| Public create/update/delete/announce/question-update | Same bytes relayed | Each configured `--relay-inbox` (`RelayForward` job); non-public, Flag, like, block, quote, profile, and actor-delete traffic is explicitly excluded |
| Move, custom emoji, `Add`/`Remove`, pins, featured-status sync | — | **No outbound activity** (verified absent) |

**Visibility → addressing:** public → `to: [Public], cc: [followers,
mentions]`; unlisted → `to: [followers], cc: [Public, mentions]`;
followers-only → `to: [followers], cc: [mentions]`; direct → `to:
[mentions], cc: []`. `Delete` of a Note/actor always uses `to: [Public]`
regardless of original visibility.

**Delivery reliability:** durable per-destination jobs, max 5 attempts,
`1s << attempts` backoff capped at 300s, 30s leases, 8 network + 8 local
in-flight workers (batch 32). Per-host FIFO sequence numbers (durable across
restarts) park a later send while an earlier one is pending/leased (park
delay 5s, max 25 parks / 10-minute age, then ordering relaxes); terminal rows
never wedge a host. Per-host failure budget: 60s cooldown doubling to 1h
after consecutive failures; 10 failures or 25 parks → quarantine; any 2xx
resets the host. Only retryable send failures charge the budget — refused
URLs, missing keys, and unparseable payloads quarantine without cooling the
host. Delivery POSTs retry on **4xx** (intended D28 divergence from the
crate default; document `GET`s still treat 404/410 as terminal). Oversize
payloads (> 64 KiB) enqueue as quarantined markers.

### Thread completion

- **Ingest-time (Mastodon-matching):** an inbound reply stored with an
  unknown parent enqueues a durable `ResolveParent` job — one hop per job,
  signed by the instance actor, same-host filter on the inline path; the
  reply stays out of home/list feeds until linked and is never promoted to
  root. Caps: 40 hops per chain, fetch budget 1000 per triggering request.
- **Read-time descendant crawl (Mastodon-matching):** an authenticated
  `GET /api/v1/statuses/:id/context` on an eligible remote distributable
  root (≥ 5 minutes old, not refreshed within 15 minutes) answers from local
  state, sets `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]`,
  and enqueues a bounded crawl (≤ 500 items per collection, ≤ 500 pages,
  ≤ 1000 replies total, breadth-first, deduped by URI, same-host filter
  intentionally absent as upstream). Anonymous reads never enqueue fetches.
- **Read-time ancestor fetch (intentional deviation D5):** the same
  authenticated context read additionally chains `ResolveParent` jobs upward
  from the first unresolved ancestor. Upstream never fetches ancestors on
  read; birchtree does, for logged-in viewers only.

## Content and extensions

- **Replies/threading:** `inReplyTo` links local/remote parents; unresolved
  parents tracked per status; context returns ancestors + descendants from
  local state (anonymous: ≤ 40 ancestors / 60 descendants / depth 20;
  authenticated: ≤ 4096 each).
- **Mentions/hashtags:** emitted as `Mention{href, name}` /
  `Hashtag{href: {public}/tags/{tag}, name: #{tag}}`; ingested with
  normalization and dedup. `Question` tags are not parsed (poll options
  carry no tags).
- **Attachments:** emitted as `Document{mediaType, url, name}` plus
  `blurhash`/`width`/`height` when known and `license` when policy allows;
  only ready (or URL-bearing) attachments are emitted. Ingested attachments
  keep source URL, media type, and description (license validated or nulled);
  **`blurhash`/dimensions are parsed but not stored** (fidelity gap).
  Remote bytes are fetched post-commit (same-origin signed, ≤ 5 redirects,
  5s connect / 10s request, 40 MB cap, MIME sniffed) and re-served verbatim;
  audio/video remotes are stream-referenced, never persisted. Local uploads
  are transcoded (GIF→WebM/VP9 `video`, video→WebM/VP9+Opus, stills→WebP;
  never `gifv` — deviations D1–D4), so remote peers observe transcoded bytes.
- **Custom emoji:** inbound only — `Emoji` tags on Notes (≤ 50, valid
  shortcode, `http(s)` icon URL ≤ 2048 chars) persist per-domain
  (`shortcode@domain`, origins never collide) and serve back on status
  payloads; bytes fetched post-commit (SSRF-checked, PNG/GIF/WebP only,
  256 KB cap). **No `Emoji` tag is ever emitted**; account-payload emoji
  stays `[]`. (Uncommitted work in progress at this revision touches the
  remote-emoji cache; see git status.)
- **Polls:** full Mastodon shape both directions (`oneOf`/`anyOf`, `endTime`
  incl. bool/millis forms, `closed`, `votersCount`); votes arrive as
  vote-Notes and tally once per voter (unknown polls/choices/blocked/expired
  votes never tally); tally changes fan out as `Update{Question}` plus a
  local `collection_update` hook.
- **Quotes:** see [FEP-044f](#supported-feps) and the activity tables.
  Local quote policy (`public`/`followers`/`nobody`) gates both outbound
  requests and inbound acceptance.
- **Content warnings/formatting:** `sensitive` + `summary` (spoiler)
  round-trip on Notes; sensitized-author statuses are forced sensitive
  (Questions inherit only the account flag — wire `sensitive` gap).
  `content` is HTML; `contentMap` carries a single language key (BCP47
  canonicalized) when set; multi-key maps are dropped.
- **Collections/pins:** `followers`/`following`/`outbox`/`featuredTags`
  served as above; conversation threading via `replies` collections during
  crawls. Status pins, endorsements, `Add`/`Remove`, and featured-status
  collections are **not** federated.
- **Move:** inbound-only partial, see table. No `Move` is ever sent; with
  `alsoKnownAs` always null, local moves cannot federate.
- **Relays/inbox forwarding:** public `Create`/`Update`/`Delete`/`Announce`/
  `Question`-update bytes forward to operator-configured `--relay-inbox`
  relays (`RelayForward`, 1 MiB cap, same retry/budget/ordering as delivery);
  inbound `Announce`/`Create` fan out per follower inbox excluding the
  arrival inbox (cap 1000 destinations). No relay subscription protocol.
- **Preview cards:** absent over federation — no `card`/`preview_url` is
  emitted or ingested. OpenGraph crawling and oEmbed exist only as
  local-only trend/embed features (signed-in, 64 KiB, sanitized).

## Security and validation

- **Signatures:** every inbox POST must carry a valid draft-Cavage
  `rsa-sha256` signature over `(request-target) host date digest
  content-type` with a `Digest: SHA-256=<base64>` body hash; missing/weak
  digest or bad/unknown key → `401` with no state change. Only the key whose
  `owner` equals the claimed actor and whose domain matches is accepted.
  Inbox body cap 1 MB; remote documents 200 KB.
- **Fetching:** allow-HTTPS-only (lab overrides refused in production),
  no userinfo, IP-literal and DNS checks against private/loopback/link-local/
  multicast/reserved ranges (incl. IPv6 mapped/`6to4`/NAT64/DOC ranges),
  any-blocked-IP DNS answers refused (rebind-safe), redirects re-validated
  per hop (delivery: none; document fetch: one manual; media: five; oEmbed:
  three), 5s connect / 10s request timeouts, ActivityStreams content-type
  required, object `id` must equal the final URL, 404/410 terminal per URI.
- **Authorization:** signatures authenticate, they do not authorize.
  Application handlers separately enforce: local-target checks, actor ==
  `attributedTo` on writes, author-equality on update/delete/quote-revoke,
  quote policy, locked-account follow gating, block/domain-suspend checks,
  `trustworthy_attribution` (object host == `attributedTo` host), and
  visibility rules on serve paths.
- **Abuse bounds:** activity-ID dedup (7d TTL), tombstone same-host rule
  (a forged `Delete` cannot poison foreign URIs), thread-crawl and parent-hop
  caps, collection page caps, per-host delivery budgets/cooldowns, bounded
  worker concurrency, no outbound HTTP inside any local state transaction.
- **Secrets:** actor private keys are sealed in the OS keyring; loss is
  unrecoverable and fail-closed. No key material is logged (`Debug` redacts).

## Known limitations

Material items a peer implementer must account for (verified at this
revision; not speculation):

1. **Lab-tested only.** Interop passes against real Mastodon 4.7.3 and a
   Misskey-family peer in a private lab; behavior against the public
   fediverse is unverified. Media exchange with Mastodon 4.7.x is flaky in
   the lab for Mastodon-side fetch/store reasons (see
   `docs/federation-contract.md` “Media interop notes”); a `Delete` racing a
   `Create` from another host resolves via tombstones, but same-host races
   depend on arrival order.
2. **Signature scope is Cavage-only.** No RFC 9421, no Ed25519/FEP-521a, no
   integrity proofs. Peers that sign *only* with RFC 9421 or Ed25519 keys
   cannot deliver to (or be fetched by) this server; such requests fail
   closed with `401`.
3. **`Undo{Announce}` is scoped to the boosted author**, not re-fanned to
   followers — remote followers keep the stale boost.
4. **`Delete` addressing is `to: [Public]`** regardless of the original
   visibility.
5. **No outbound `Move`, emoji, pins, `Add`/`Remove`, or featured-status
   sync.** Remote peers never receive `Emoji` tags (custom emoji renders as
   plain `:shortcode:` text remotely) and never observe local migrations.
   `alsoKnownAs` is never emitted or checked — treat inbound `Move`s as
   target-deref-only assertions.
6. **Follower/following counts are hints**, never reconciled; no FEP-8fcf
   sync. No sever cascade (`severed_relationships` producer absent).
7. **Content fidelity gaps:** `blurhash`/media dimensions parsed but not
   stored; `Question` tags and `sensitive` not ingested; multi-key
   `contentMap` dropped; `quoteUrl` (new Misskey key) not parsed; no
   `preview_url`/`card`; transcoded local media (D1–D4) changes bytes/URLs/
   `meta` that remote caches keyed on originals will miss.
8. **Delivery retries on 4xx** (intended D28): a peer returning `4xx` to a
   delivery POST is retried under the job budget before quarantine, not
   treated as instant-terminal.
9. **Read-path deviations:** authenticated context reads may trigger
   ancestor fetches (D5 — upstream never does); logged-out context reads
   never fetch; home timelines read only the 1000 most recent follows
   (D10a) with no `206` async-refresh (D9); direct timelines depend on local
   conversation state (D18).
10. **Uncertainty:** exact `@context` serialization order (uses the crate's
    `WithContext` wrapper), WebFinger/cache-control headers, and strict
    parity of the authorized-fetch endpoint set with Mastodon's
    `require_account_signature!` list were not byte-verified. The
    `adv2_*` remote-media/cache/emoji/ordering test files and several
    `src/` + `docs/` edits were uncommitted at audit time; behavior in those
    areas may shift once that work lands.

## Additional documentation

All paths are relative to the repository root (verified present at this
revision):

- `docs/federation-contract.md` — the normative federation contract
  (ingress/egress, delivery budgets, tombstones, thread completion, signed
  fetch, media interop notes).
- `ACTIVITYPUB-FEDERATION-RUST.md` — crate role boundary, Mastodon
  compat checklist, and the required signature/key spike.
- `docs/compatibility-profile.md` — deviation register D1–D38, including
  all federation deviations cited above.
- `docs/configuration.md` — operator flags (`--federation-*`,
  `--relay-inbox`, remote-cache, keyring, collection visibility).
- `docs/architecture.md` — adapter layout and transaction/durability rules.
- `docs/phase-6-exit-criteria.md` — federation phase scope and lab evidence.
- `docs/federation_repair.md` — closed boost-domain bug + open `remote_url`
  investigation.
- `scripts/lab/README.md` — the two-instance Mastodon 4.7.3 interop lab.
- `docs/logs/2026-10-17-phase-6-interop-run.out`,
  `docs/logs/2026-10-18-phase-6-interop-run.out` — executed interop runs.
- `tests/phase6_federation.rs`, `tests/phase6_content.rs`,
  `tests/phase6_gaps_round2_e.rs`, `tests/gaps_federation_tail.rs`,
  `tests/fix_delivery_order.rs`, `tests/a_signed_media_fetch.rs`,
  `tests/fix_fed_policy.rs`, `tests/fix4_p2_fedpush.rs` — executable
  federation evidence.
