# 🌳 birchtree

> A single-process, Mastodon-API-compatible fediverse server, written in Rust.

birchtree reimplements [Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
(API v7) in Rust, backed by embedded `redb` (no Postgres/Redis) and local-disk
media. Federation speaks ActivityPub via
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
plus birchtree's own policy and delivery rules.

birchtree is **not a full drop-in replacement for Mastodon yet**. It is local-usable
and federating in the lab (see below), but several API surfaces are still missing
and it has only been tested in simulated environments and a private lab — not
against the live fediverse. YMMV.

birchtree does **not** come with a frontend or client. See
[keystone](https://github.com/estyseesghosts/keystone) (in development). Any
Mastodon-compatible client should work for the flows listed below; I have not
tested the official Mastodon frontend against it yet. YMMV.

- **Language:** Rust (edition 2024)
- **Storage:** embedded `redb` (single file, single node — no Postgres/Redis needed)
- **Media:** local filesystem
- **License:** AGPL-3.0

---

## ✨ What works today

Status as of 2026-10-20 (`cargo test --release`: 349 passed / 0 failed).
Unimplemented routes return a clean `404` — never fake data — with the
exceptions noted below (endpoints that correctly answer empty because their
data source doesn't exist yet).

- **Accounts & auth** — OAuth2 app registration, authorization-code flow with
  PKCE S256, client-credentials apps, token revoke, per-route scope enforcement,
  server-rendered login/consent pages, registration (open/closed policy), CLI
  bootstrap account. Not there: email-confirmation enforcement, invite/approval
  registration modes, refresh tokens (`unsupported_grant_type`).
- **Statuses** — post, show, edit (with history), delete, replies + context,
  reblogs, favourites, bookmarks, pins (limit 5), `favourited_by`/`reblogged_by`,
  source view, multi-fetch, quotes (local state machine: policy, accept/revoke,
  interaction-policy endpoint), local polls (create/vote/expiry, hidden totals),
  scheduled statuses (with `<5 min` 422 parity), `Idempotency-Key` replay.
  Status text limit defaults to 500 chars (Mastodon parity), configurable to
  3000 via `--max-status-chars=`.
- **Relationships** — follow (locked accounts → pending requests with
  authorize/reject), unfollow, remove-from-followers, block, mute (with expiry),
  domain blocks (upstream v4.7.3 shape), endorsements, account notes,
  followers/following lists, per-viewer relationship aggregates. Not there:
  `GET blocks/mutes/favourites/bookmarks` collection reads.
- **Media** — v1/v2 multipart upload with MIME sniffing, processing lifecycle
  (`pending → ready`, `202`/`206` semantics), description/focus updates, local
  file serving with sniffed content types. **No transcoding yet** (no ffmpeg):
  animated GIFs stay `image` (never `gifv` — deliberate deviation D1),
  video/audio bytes pass through untouched, stills are not converted to WebP
  (deviations D2–D4, planned Phase 7.1).
- **Timelines** — home, public (`local`/`remote`/`only_media`), hashtag, list,
  and account timelines with exclusive numeric cursors and `Link` headers.
  Accepted deviations: home reads only the 1000 most recent follows (D10a),
  exclusive-list members are hidden from home (D10b), no `206` async-refresh on
  home (D9). No frontend, no full-text engine: search is `GET /api/v2/search`
  over the local corpus only (accounts/statuses/hashtags).
- **Notifications, markers, lists, filters** — notification index/show/clear/
  dismiss/`unread_count`, `GET`+`POST /api/v1/markers`; full lists CRUD +
  membership; filters v1+v2 with `filtered` annotation. Gaps: notification
  `requests` always `[]` (no request-filter pipeline), no `notifications_merged`
  event, chosen-languages filtering not applied.
- **Streaming** — in-process SSE and WebSocket for `user`, `user:notification`,
  `public` (+`local`/`remote`/media variants), `hashtag` (+`local`), and `list`,
  with auth/scopes, heartbeats, and explicit connection bounds (512 total,
  16/token, fail-closed overflow). `direct` is accepted but silent until
  conversations exist (D18).
- **Federation** — WebFinger, NodeInfo 2.0, actor/key documents, shared +
  personal inboxes, outbox/followers/following collections, signed delivery
  with retry/backoff/quarantine, SSRF-safe fetching. Activities both
  directions: Follow/Accept/Reject/Undo, Create/Update/Delete Note, Like/Undo,
  Announce/Undo (including Undo on reblog-delete), Block/Undo, Question polls
  + votes, quote Request/Accept/Reject/Revoke, Flag send/receive, remote media
  fetch (original bytes cached verbatim). Thread completion: ingest-time parent
  resolution plus authenticated context crawl (with a logged-in-only ancestor
  fetch Mastodon doesn't do — deviation D5). Lab-verified against real
  Mastodon 4.7.3 and a five-server bench (posts/replies/boosts/likes/deletes/
  media/follows pass vs self, pinetree, and Mastodon).
- **Already-in from Phase 7** — account multi-fetch (`?id[]=`, up to 40),
  lookup and search; `POST /api/v1/reports` (stores the report, forwards `Flag`
  to remote targets when asked); suggestions and announcements answer `[]`
  (no data sources wired yet); donation campaigns answer `204`.

---

## 🚧 What birchtree can NOT do yet

- **Conversations / DMs UI surface** — no `GET /api/v1/conversations` family,
  no conversation mutes; direct messages federate as statuses but have no
  conversation threading or `direct`-stream events.
- **Web Push** — no push subscriptions or delivery (planned Phase 7).
- **Admin & moderation** — no `/api/v1/admin/*` or `/api/v2/admin/*`, no roles
  (CLI accounts are role-less), no report queue UI, no trends, no annual
  reports (planned Phase 7.3).
- **First-party web client** — no `/api/web/*`, no keystodon bundle serving
  (planned Phase 7).
- **Media transcoding** — as above: no GIF→video, no VP9/WebM, no WebP, no
  `gifv` (planned Phase 7.1).
- **Real search** — local-corpus scan only; no embedded full-text engine, no
  Elasticsearch (planned Phase 7.2).
- **Multi-node / clustering** — single node only; no Postgres/Redis/FoundationDB
  backend, no shared-storage media arrangement.
- **Known federation rough edges** — media exchange with Mastodon 4.7.x nodes is
  flaky in the lab (Mastodon-side fetch/re-encode behavior, documented in
  `docs/federation-contract.md`; interop vs real Mastodon passes); no
  per-destination delivery cooldown yet (backoff is per-job); a Delete that races
  a Create can leave behind residue (needs tombstones, the one known
  birchtree-caused issue); email sending is a logging stub (no SMTP).

If a client needs any of the above, birchtree is not ready for it. Everything
else in the previous section is implemented, tested, and — for federation —
measured against real Mastodon 4.7.3 in the lab.

---

## 🚀 Run your own birchtree server

You don't need Docker, Postgres, or Redis. One binary, one data directory.

### 1. Prerequisites

- **Rust toolchain** (stable, edition 2024). Install via [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **A C toolchain** (gcc/clang) — pulled in by Rust's crypto deps. On Fedora:
  `sudo dnf install gcc`. On Debian/Ubuntu: `sudo apt install build-essential`.
- `curl` for poking the API.

Check your toolchain:
```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### 2. Get the code

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
```

### 3. Build

```bash
cargo build --release
```

The binary lands at `target/release/birchtree`. (A debug build works too:
`cargo build`, then use `target/debug/birchtree`.)

### 4. Pick a data directory and public URL

birchtree stores everything in one directory: the `redb` database file plus
uploaded media. Choose a path you own, e.g. `~/.local/share/birchtree`.

The **public URL** is how the rest of the fediverse (and your clients) reach
you. For local tinkering, `http://localhost:3000` is fine. For a real instance,
use your HTTPS domain, e.g. `https://social.example.com`.

### 5. Start the server

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

You should see a log line like `birchtree listening`. Check it's alive:

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

Stop it with `Ctrl-C` (it shuts down gracefully).

### 6. Create your admin account

Registration is **closed by default**, so bootstrap the first account from the
CLI:

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

It prints `created account id=... username=alice`. That account is your admin.

> 💡 You can also pass the password via the `BIRCHTREE_PASSWORD` env var
> instead of `--password=`.

### 7. (Optional) Open registration

To let people sign up, add `--registration-open`:

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### 8. Connect a client

Point any Mastodon client at `http://localhost:3000` (or your public URL).
Create an app first, then log in:

```bash
# 1. Register an OAuth app
curl -s -X POST http://localhost:3000/api/v1/apps \
  -d 'client_name=myclient' \
  -d 'redirect_uris=urn:ietf:wg:oauth:2.0:oob' \
  -d 'scopes=read write follow push'

# 2. Then use the returned client_id / client_secret in your client's login flow.
```

The instance info is at `GET /api/v1/instance` and `GET /api/v2/instance`.

### 9. Run it as a service (systemd example)

Create `/etc/systemd/system/birchtree.service`:

```ini
[Unit]
Description=birchtree fediverse server
After=network.target

[Service]
Type=simple
User=birchtree
ExecStart=/usr/local/bin/birchtree \
  --data-dir=/var/lib/birchtree \
  --listen=127.0.0.1:3000 \
  --public-url=https://social.example.com
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Then:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now birchtree
```

Put a reverse proxy (Caddy/nginx) in front for HTTPS — birchtree itself speaks
plain HTTP and trusts `X-Forwarded-For` only from addresses you list with
`--trusted-proxy=`.

---

## ⚙️ Configuration reference

All options are command-line flags (there's no config file). Defaults are shown.

| Flag | Default | What it does |
|---|---|---|
| `--public-url=` | `http://localhost:3000` | Public base URL used for actor/object URIs. Must be `http://` or `https://`. |
| `--data-dir=` | `./data` | Directory for the database + media. |
| `--listen=` | `0.0.0.0:3000` | Bind address. |
| `--trusted-proxy=` | (none) | IP allowed to set `X-Forwarded-For`. Repeatable. |
| `--max-body-size=` | `16777216` | Max request body bytes (also caps media uploads). |
| `--max-status-chars=` | `500` | Max status text length in characters (URLs count as 23). Mastodon parity at the default; up to `3000` (Misskey parity). |
| `--site-title=` | `Birchtree` | Instance title. |
| `--site-description=` | `A Mastodon-compatible server` | Instance description. |
| `--site-contact-email=` | `admin@localhost` | Contact email. |
| `--site-contact-username=` | (none) | Contact account handle. |
| `--registration-open` / `--registration-closed` | closed | Toggle sign-ups. |
| `--streaming-url=` | (none) | Advertise a separate streaming host. |
| `--federation-allow-http` | off | Allow `http://` federation targets (lab only). |
| `--federation-allow-private=` | (none) | CIDR allowlist for private federation targets (lab only). |
| `--federation-extra-ca=` | (none) | Extra CA bundle (PEM) for federation TLS (lab only). |
| `--federation-authorized-fetch` | off | Require signatures on note/collection reads. |

Environment: `RUST_LOG` controls log verbosity, e.g. `RUST_LOG=info,birchtree=debug`.

---

## 🧪 Development & tests

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

The repo uses a phase-by-phase plan (`birchtree.md`). Contracts live in `docs/`,
and `state/logs/` has dated work logs. See `docs/wiki/` for a readable overview.

### Federation lab (optional)

To test federation against a real Mastodon, see `scripts/lab/README.md`. It spins
up a private Docker bridge with Mastodon 4.7.3. Lab switches are off by default
and must never be used in production.

---

## 📚 Further reading

- `birchtree.md` — the full plan (pinned to Mastodon v4.7.3).
- `docs/` — contracts (endpoints, domain types, visibility, persistence, …).
- `docs/wiki/` — About, Current state, Roadmap, Feature matrix.
- `AGENTS.md` — rules for agents working in this repo.

---

Made with 🌿 and a lot of `cargo test`. Questions, bugs, ideas welcome.
