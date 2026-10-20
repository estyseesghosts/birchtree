# birchtree

birchtree is a single-process, **Mastodon-API-compatible fediverse server**
written in Rust. It reimplements [Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
(API v7) backed by embedded `redb` and local-disk media. Federation speaks
ActivityPub via
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
plus birchtree's own policy and delivery rules.

It is deliberately **small to run**: no Postgres, no Redis, no message broker.
One binary, one data directory (`redb` file + media), done.

birchtree does not come with a frontend. Point any Mastodon-compatible client
at it — see [keystone](https://github.com/estyseesghosts/keystone) (in
development). The official Mastodon frontend has not been tested against it
yet. YMMV.

> **Status (2026-10-20):** local-usable and federating in the lab
> (`cargo test --release`: 349 passed / 0 failed). Accounts, statuses,
> relationships, media, timelines, notifications/markers/lists/filters,
> streaming (SSE + WebSocket), and core federation are implemented and
> lab-verified against real Mastodon 4.7.3. Conversations, Web Push, admin
> API, `/api/web/*`, transcoding, full-text search, and multi-node are
> planned, not built. Tested in simulated environments and a private lab
> only — never against the live fediverse.

---

## Table of contents

- [What it does today](#what-it-does-today)
- [What it does not do](#what-it-does-not-do)
- [Known gaps and rough edges](#known-gaps-and-rough-edges)
- [Where things stand](#where-things-stand)
- [Run your own server](#run-your-own-server)
  - [Prerequisites](#prerequisites)
  - [Build it](#build-it)
  - [Start it](#start-it)
  - [Create your first account](#create-your-first-account)
  - [Log in and post](#log-in-and-post)
- [Use it day-to-day](#use-it-day-to-day)
- [What it talks to](#what-it-talks-to)
- [Project layout](#project-layout)
- [Testing it yourself](#testing-it-yourself)
- [Contributing & reading more](#contributing--reading-more)
- [License](#license)

---

## What it does today

Everything in this section runs on the single-node server and is covered by
executed tests. Unimplemented routes return a clean `404` — never fake data —
with the exceptions noted below (endpoints that correctly answer empty
because their data source does not exist yet). The exact contract is
`docs/endpoint-manifest.md` plus `docs/compatibility-profile.md` (deviation
register D1–D18).

**Accounts, auth, and registration:**

- OAuth2 app registration (`POST /api/v1/apps`), authorization-code flow with
  PKCE S256, client-credentials apps, token revoke, per-route scope
  enforcement, server-rendered login/consent pages.
- Registration policy (open/closed), CLI bootstrap account. Email sending is a
  logging stub — no enforcement.
- Mastodon-compatible decimal IDs: 64-bit timestamp-oriented IDs serialized as
  decimal strings; numeric comparison internally, never lexicographic.

**Statuses and interactions:**

- Post, show, edit (with history), delete, replies + context, reblogs,
  favourites, bookmarks, pins (limit 5), `favourited_by`/`reblogged_by`,
  source view, multi-fetch, `Idempotency-Key` replay (1-hour retention).
- Quotes as a local state machine: policy (`public`/`followers`/`nobody`),
  accept/revoke, interaction-policy endpoint, `quotes_count`.
- Local polls (create/vote/expiry, hidden totals) and scheduled statuses
  (with `<5 min` 422 parity).
- Status text limit defaults to 500 chars (Mastodon parity), configurable to
  3000 via `--max-status-chars=`.

**Relationships:**

- Follow (locked accounts → pending requests with authorize/reject),
  unfollow, remove-from-followers, block, mute (with expiry), domain blocks
  (upstream v4.7.3 shape), endorsements, account notes, followers/following
  lists, per-viewer relationship aggregates.

**Media:**

- v1/v2 multipart upload with MIME sniffing, processing lifecycle
  (`pending → ready`, `202`/`206` semantics), description/focus updates,
  local file serving with sniffed content types.

**Timelines and search:**

- Home, public (`local`/`remote`/`only_media`), hashtag, list, and account
  timelines with exclusive numeric cursors and `Link` headers.
- Search is `GET /api/v2/search` over the local corpus only
  (accounts/statuses/hashtags) — no full-text engine.

**Notifications, markers, lists, filters:**

- Notification index/show/clear/dismiss/`unread_count`,
  `GET`+`POST /api/v1/markers`; full lists CRUD + membership; filters v1+v2
  with `filtered` annotation.
- Already-in from Phase 7: account multi-fetch (`?id[]=`, up to 40), lookup
  and search; `POST /api/v1/reports` (stores + forwards `Flag` when asked);
  suggestions and announcements answer `[]`; donation campaigns answer `204`.

**Streaming:**

- In-process SSE and WebSocket for `user`, `user:notification`, `public`
  (+`local`/`remote`/media variants), `hashtag` (+`local`), and `list`, with
  auth/scopes, heartbeats, and explicit bounds (512 total, 16/token,
  fail-closed overflow).
- Events: `update`, `delete`, `status.update`, `notification`.

**Federation (ActivityPub):**

- WebFinger (`acct:` resolution), NodeInfo 2.0, actor/key documents, shared +
  personal inboxes, outbox/followers/following collections, signed delivery
  with retry/backoff/quarantine, SSRF-safe fetching (size/timeout/redirect
  limits, DNS/IP defenses).
- Activities both directions: Follow/Accept/Reject/Undo, Create/Update/Delete
  Note, Like/Undo, Announce/Undo (including Undo on reblog-delete),
  Block/Undo, Question polls + votes, quote Request/Accept/Reject/Revoke,
  Flag send/receive, remote media fetch (original bytes cached verbatim).
- Thread completion: ingest-time parent resolution (one hop per job, hop cap
  + fetch budget, never promote to root) plus authenticated context crawl
  with `Mastodon-Async-Refresh`.

**Durability:**

- One `redb` transaction commits state mutation + indexes/counters +
  idempotency/dedup records + bounded durable jobs atomically. Network I/O,
  media processing, notifications fan-out, and delivery happen after commit.
  Workers are idempotent; crash-recovery tests prove no committed mutation
  loses its job.

---

## What it does not do

Deliberate non-goals and deferred work — asking for these returns `404`
(or the documented empty/204 below) rather than a broken half-implementation:

- **No conversations / DMs UI surface.** No `GET /api/v1/conversations`
  family, no conversation mutes. Direct messages federate as statuses but
  have no threading or `direct`-stream events.
- **No Web Push.** No push subscriptions or delivery (planned Phase 7).
- **No admin & moderation API.** No `/api/v1/admin/*` or `/api/v2/admin/*`,
  no roles (CLI accounts are role-less), no report queue UI, no trends, no
  annual reports (planned Phase 7.3).
- **No first-party web client routes.** No `/api/web/*`, no keystodon bundle
  serving (planned Phase 7).
- **No media transcoding.** No GIF→video, no VP9/WebM, no WebP, no `gifv`
  (planned Phase 7.1).
- **No real search.** Local-corpus scan only; no embedded full-text engine,
  no Elasticsearch (planned Phase 7.2).
- **No multi-node mode.** Single node only; no Postgres/Redis/FoundationDB
  backend, no shared-storage media arrangement.
- **Not there yet:** email-confirmation enforcement, invite/approval
  registration modes, refresh tokens (`unsupported_grant_type`), `GET
  blocks/mutes/favourites/bookmarks` collection reads, `notifications_merged`
  event, notification `requests` pipeline (always `[]`), chosen-languages
  filtering.

---

## Known gaps and rough edges

Things that work but with documented limits you may trip over:

- **Lab-tested only.** Interop is verified against real Mastodon 4.7.3 in a
  private Docker bridge lab, never against the live public internet. Lab
  switches (`--federation-allow-http`, `--federation-allow-private`,
  `--federation-extra-ca`) are off by default and must never be used in
  production.
- **Media exchange with Mastodon 4.7.x is flaky in the lab**
  (Mastodon-side fetch/re-encode behavior; see
  `docs/federation-contract.md`). No per-destination delivery cooldown yet
  (backoff is per-job). A Delete that races a Create can leave residue
  (needs tombstones — the one known birchtree-caused issue).
- **Media deviations (accepted):** animated GIFs stay `image`, never `gifv`
  (D1); video/audio pass through untouched (D2–D3); stills not converted to
  WebP (D4). Phase 7.1 changes the packer (`type`, `url`, `preview_url`,
  `meta`, null-while-pending).
- **Timeline deviations (accepted):** home reads only the 1000 most recent
  follows (D10a); exclusive-list members hidden from home (D10b); no `206`
  async-refresh on home (D9).
- **Thread deviation (D5):** logged-in-only ancestor fetch on context read —
  Mastodon does not do this.
- **`direct` stream is accepted but silent** until conversations exist (D18).
- **Single-node storage.** One writer (`redb`), no read replicas, no live
  migration path to multi-node.
- **Minimal observability** (structured logs, `RUST_LOG`, health/readiness
  only). No per-destination delivery tuning UI.

---

## Where things stand

| Phase | What it was | Status |
|---|---|---|
| 0 | Freeze source contracts (route inventory vs v4.7.3, DTOs, visibility, ports) | In review — route-inventory review open |
| 1 | Runtime, `redb`, IDs, config, bootstrap | Done |
| 2 | OAuth, registration, exact read APIs | Done |
| 3 | Local statuses, relationships, quotes, idempotency | Done |
| 4 | Media, timelines, notifications, lists/filters/search | Done |
| 4.1 | Parity gap closure (JSON bodies D11, exclusive lists D10b, home cap D10a) | Done |
| 5 | Streaming (SSE + WebSocket, envelopes, bounds) | Current — largely built, exit review open |
| 6 | ActivityPub federation (inboxes, delivery, thread completion) | Core landed + lab-verified; hardening open |
| 7 | Transcoding (7.1), search (7.2), admin/moderation (7.3), push, `/api/web/*` | Planned — only multi-fetch/reports/stubs landed |
| 8 | Declared API compatibility (98% parity suite) | Planned |

The single-node server is **local-usable today**. Declared v4.7.3
compatibility is not claimed until the Phase 8 parity gate passes.

---

## Run your own server

### Prerequisites

1. **Rust toolchain** (stable, edition 2024) via [rustup](https://rustup.rs/).
2. **A C toolchain** (gcc/clang) for crypto deps. Fedora:
   `sudo dnf install gcc`. Debian/Ubuntu: `sudo apt install build-essential`.
3. `curl` for poking the API.

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### Build it

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

This produces a single binary at `target/release/birchtree`
(`target/debug/birchtree` for `cargo build`).

### Start it

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` is the URL the world (and clients) sees. `http://` or
  `https://` with a host. Actor/object URIs, WebFinger, and NodeInfo build
  from it. `http://localhost:3000` is fine for tinkering;
  `https://social.example.com` for a real instance.
- `--data-dir` holds the `redb` file + uploaded media. Point it at a
  directory you can back up. Default `./data`.
- `--listen` is the bind address. `127.0.0.1:3000` keeps it local-only.

Check it is alive:

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

Stop with `Ctrl-C`. For production put a reverse proxy (Caddy/nginx) in
front for HTTPS — birchtree speaks plain HTTP and trusts `X-Forwarded-For`
only from `--trusted-proxy=` addresses.

Full flag reference (`--max-body-size`, `--max-status-chars`,
`--site-title/description/contact`, `--registration-open/closed`,
`--streaming-url`, `--federation-*`, `RUST_LOG`): see `--help` and the table
in the previous README revision / `src/config.rs`.

### Create your first account

Registration is **closed by default**, so bootstrap the first account from
the CLI:

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

It prints `created account id=... username=alice`. That account is your
admin. Password may come from `BIRCHTREE_PASSWORD` instead of `--password=`.

To open sign-ups:

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### Log in and post

Point any Mastodon client at your public URL, or use the API directly:

```bash
# 1. Register an OAuth app
curl -s -X POST http://localhost:3000/api/v1/apps \
  -d 'client_name=myclient' \
  -d 'redirect_uris=urn:ietf:wg:oauth:2.0:oob' \
  -d 'scopes=read write follow push'

# 2. Use client_id / client_secret in the authorization-code + PKCE flow,
#    then post with the user token:
curl -s -X POST http://localhost:3000/api/v1/statuses \
  -H 'Authorization: Bearer <token>' \
  -d 'status=hello from birchtree&visibility=public'
```

Instance info: `GET /api/v1/instance` and `GET /api/v2/instance`.

---

## Use it day-to-day

Once running, interact like any Mastodon instance (form-encoded, multipart,
or JSON bodies; IDs are decimal strings; cursors are exclusive numeric
`max_id`/`since_id` with `Link` headers):

- **Post:** `POST /api/v1/statuses` with `status`, `visibility`
  (`public`/`unlisted`/`private`/`direct`), `spoiler_text`, `in_reply_to_id`,
  `media_ids[]`, `poll[options][]`, `scheduled_at`, `quote` fields. Edit via
  `PUT /api/v1/statuses/:id`, delete via `DELETE /api/v1/statuses/:id`.
- **Timelines:** `GET /api/v1/timelines/home|public`, `/api/v1/timelines/tag/:tag`,
  `/api/v1/timelines/list/:id`, `/api/v1/accounts/:id/statuses`.
- **Interact:** `/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`,
  `/api/v1/polls/:id/votes`, `/api/v1/follows`, `/api/v1/blocks|mutes|domain_blocks`.
- **Media:** `POST /api/v1/media` / `POST /api/v2/media` (multipart); files
  served locally with sniffed content types.
- **Notifications/markers:** `/api/v1/notifications`, `/api/v1/markers`.
- **Lists/filters:** `/api/v1/lists`, `/api/v1/filters` (v1+v2).
- **Search:** `GET /api/v2/search` (local corpus only).
- **Streaming:** SSE or WebSocket on the streaming endpoint with OAuth token;
  streams `user`, `user:notification`, `public`, `hashtag`, `list`.

The full surface is in `docs/endpoint-manifest.md`. Anything not in profile
returns `404` with Mastodon's `{ "error": "..." }` shape (not a Misskey
envelope).

---

## What it talks to

birchtree federates over ActivityPub and has been tested in a private lab —
never against the public internet:

- **Mastodon 4.7.3** (pinned commit `8ff11c2b`): follows, posts, replies,
  boosts, likes, deletes, media, polls, quotes pass in a five-server bench
  (self, pinetree, Mastodon).
- **pinetree** (Misskey-family): cross-family bench passes for the core
  activity set.

It speaks Create/Update/Delete Note, Follow/Accept/Reject/Undo, Like/Undo,
Announce/Undo, Block/Undo, Flag, Question + votes, and quote
Request/Accept/Reject/Revoke, plus WebFinger, NodeInfo 2.0, and HTTP
Signatures. Federation input is treated as untrusted (size/timeout/redirect
limits, SSRF protection).

---

## Project layout

```
.
├── birchtree.md               product & architecture plan (source of truth, pins v4.7.3)
├── AGENTS.md                  rules for anyone (human or AI) working in this repo
├── docs/                      contracts (Status: Draft/Frozen) + wiki + logs/
│   ├── endpoint-manifest.md   the Mastodon API contract
│   ├── compatibility-profile.md  deviation register D1–D18
│   ├── federation-contract.md streaming-contract.md, visibility, ports, ...
│   └── wiki/                  living overview (About, Current-State, Roadmap, Feature-Matrix)
├── src/                       the Rust crate (one modular monolith)
│   ├── domain/                pure types & rules (no HTTP, no DB)
│   ├── application/           use cases (commands, queries, ports)
│   ├── api/mastodon/          HTTP adapter (router, DTOs, pagination, errors)
│   ├── activitypub/           protocol adapter (actor, objects, parser, inbox)
│   ├── federation/            resolver, delivery, retry, dedup, remote media
│   ├── persistence/redb/      embedded database adapter
│   ├── streaming/             hub, SSE, WebSocket
│   ├── jobs/                  durable job queue & worker
│   ├── media/                 ingest, blob store, transcode stub
│   └── config.rs              CLI flags (no config file)
├── tests/                     api_contract, oauth, persistence_conformance,
│                              crash_recovery, federation_interop, streaming
├── scripts/                   smoke-all.sh, bench-local.py, lab/
└── state/logs/                dated work logs with real command output
```

Dependency direction is `domain` ← `application` ← adapters. REST and
ActivityPub call the same application operations — no duplicated business
logic. Federation itself is delegated to `activitypub-federation-rust`;
`federation/` is policy/delivery, not a custom protocol implementation.

---

## Testing it yourself

The full verification gate is:

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

You can also run pieces individually (`cargo test <name>`). See
`docs/testing.md` for the full guide.

To test federation against a real Mastodon, see `scripts/lab/README.md`. It
spins up a private Docker bridge with Mastodon 4.7.3. Lab switches are off
by default and must never be used in production.

---

## Contributing & reading more

- `birchtree.md` — the product and architecture plan. Wins on any
  disagreement with this README.
- `AGENTS.md` — the rules of the road for contributors (human or AI).
- `docs/` — the contracts (`endpoint-manifest.md`, `compatibility-profile.md`,
  `federation-contract.md`, `streaming-contract.md`, …) each with a
  `Status:` line. Frozen files change only via a logged superseding decision.
- `docs/wiki/` — readable overview (About, Current state, Roadmap).
- `state/logs/` + `docs/logs/` — dated work logs and verification evidence
  with real output. Read the latest entry to get up to speed.

If you want to help, the most useful things are: trying it out against a
Mastodon-compatible client, running the test suite and the federation lab,
and filing issues. Transcoding (7.1), search (7.2), and admin/moderation
(7.3) are the open frontiers.

---

## License

AGPL-3.0 — see `LICENSE`.
