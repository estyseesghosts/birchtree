# 🌳 birchtree

> A friendly, single-process Mastodon-compatible server, written in Rust.

birchtree is a backend for the fediverse: it speaks the same API that Mastodon
does, so unmodified Mastodon clients (and the web) can talk to it. You point a
client at your birchtree instance and it just works — local posting, timelines,
notifications, media, streaming, the lot. Federation (talking to other servers)
is implemented too, though it's still being hardened.

There's **no frontend in this repo** — birchtree is backend-only. The web UI is
a separate project called [keystone](https://github.com/estyseesghosts/keystone), which can run as birchtree's first-party
client or as a third-party client against any host.

- **Language:** Rust (edition 2024)
- **Storage:** embedded `redb` (single file, single node — no Postgres/Redis needed)
- **Media:** local filesystem
- **License:** AGPL-3.0

---

## ✨ What works today

A lot, honestly. The honest list:

- **Accounts & auth** — OAuth2 (PKCE), registration (open/closed), login/consent
  flow, scopes, token revoke.
- **Statuses** — post, edit, delete, reply, reblog, favourite, bookmark, pin,
  polls, quotes, scheduled statuses, idempotent retries.
- **Relationships** — follow (with locked-account requests), block, mute (with
  expiry), domain blocks, endorsements, account notes.
- **Media** — v1/v2 multipart upload, image/video/audio, processing lifecycle,
  serving bytes with correct content type.
- **Timelines** — home, public (local/remote/only-media), tag, list; cursors
  and `Link` headers just like Mastodon.
- **Notifications & markers** — full read surface, unread counts.
- **Lists & filters** — custom filters on statuses and notifications.
- **Search** — accounts and statuses (local corpus today; a real engine is planned).
- **Streaming** — SSE and WebSocket for `user`, `user:notification`, `public`,
  `hashtag`, `list`, `direct` channels, with heartbeats.
- **Federation** — ActivityPub follow/like/announce/block/quote/flag family in
  both directions, remote media, thread completion. Lab-tested against a real
  Mastodon 4.7.3.
- **CLI** — `serve`, `create-account`.

What's **not** there yet: Web Push, conversations, announcements storage,
admin/moderation API, media transcoding, and a few smaller surfaces. Unimplemented
routes return a clean `404` — never fake data.

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
