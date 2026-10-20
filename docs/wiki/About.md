# About birchtree

birchtree is a place to run your own social network server. Regular Mastodon apps — the programs people already use to post, follow, and read timelines — can connect to it and feel at home. That is the whole point: you run this server, and people use the apps they know.

In plain terms: birchtree stores accounts and posts, decides who may see what, and talks to other servers in the same shared language, so someone on your server can follow and reply to someone on another server.

## How it is built

- One program does the work. You start a single process and it serves the site.
- Storage is a built-in file on the server. There is no separate database to install or run. (The storage engine is called redb.)
- Pictures and videos are kept as files on the same server.
- Background chores — sending deliveries, finishing uploads, fanning out notifications — run inside the same process as small durable jobs. Each job is saved together with the change it follows up on, so a crash cannot lose the follow-up work.

<!-- src/persistence/redb/mod.rs, src/jobs/queue.rs, src/media/blob_store.rs, src/config.rs:170-176 -->

## What "works with Mastodon" means here

The goal is simple: an ordinary Mastodon app connects to birchtree and gets the same answers it would get from Mastodon version 4.7.3 — the same posts, the same timelines, the same error messages, the same live updates. Where birchtree deliberately differs, the difference is written down in the deviation register (`docs/compatibility-profile.md`). Anything not written down there counts as a bug.

<!-- birchtree.md:4-6, docs/compatibility-profile.md -->

## What you can do with it today

- People can register (when you open registration), sign in, and connect apps through the standard sign-in handshake (OAuth, the same "approve this app" flow Mastodon uses).
- People can post, edit, delete, reply, re-post (boost), like (favourite), save (bookmark), pin, vote in polls, and use quote posts with the author's approval rules.
- People can follow, block, mute, and manage lists and filters. Timelines (home, local, tag, list) page the same way Mastodon apps expect.
- Media can be uploaded and served; pictures and clips are converted to open formats behind the scenes (see Current-State).
- Live updates stream to apps over two transports (SSE, a one-way stream, and WebSocket, a two-way socket).
- Servers can find each other (WebFinger, a "where is this account?" lookup; NodeInfo, a "what is this server?" card) and exchange follows, posts, likes, boosts, and deletes.
- Operators get moderation and statistics tools: staff accounts, reports, trend approvals, yearly summaries, and usage measures.

<!-- src/api/mastodon/router.rs:189-206,270-282,306-345,465-543,565-567; src/activitypub/http.rs:42-44; src/streaming/hub.rs:60-63; src/search/mod.rs:6-21; src/media/transcode.rs:1-22 -->

## How to run it

You need a server machine and a name (a web address) for it. Settings live in an optional settings file plus command-line flags; flags win over the file, and the server checks the result before starting. Changing settings means stopping and restarting the server — there is no "reload while running" mode, by decision.

<!-- src/config.rs:21-28,341-349; src/main.rs:38 -->

```bash
# Start the server (storage in ./data, public name set explicitly)
/birchtree serve --public-url=https://example.social --data-dir=./data

# Make the first account (always available, even when public sign-up is closed)
/birchtree create-account --username=alice --password=...
```

Health checks: `GET /health` says the process is alive; `GET /ready` says storage is open and ready.

<!-- src/api/mastodon/router.rs:600-612; src/main.rs:193 -->

## The companion app

birchtree ships no in-house app source code. The login-and-approve pages it renders are minimal on purpose: just enough for the standard sign-in flow. The planned everyday app is **keystodon**, built separately: birchtree can serve its ready-made files, and keystodon can also connect to any server as an ordinary third-party app.

<!-- birchtree.md:7 -->

## Where the details live

- `birchtree.md` — the plan, pinned to Mastodon 4.7.3.
- `docs/` — the written contracts (endpoint list, data shapes, visibility rules, federation and live-update contracts).
- `docs/wiki/` — this readable overview (About, Current-State, Roadmap, Feature-Matrix).
- `state/logs/` and `docs/logs/` — dated notes recording what was built and what the test runs showed.

<!-- docs/README.md:6-32 -->
