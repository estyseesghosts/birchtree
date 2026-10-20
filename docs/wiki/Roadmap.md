# Roadmap

This page is the plain-words map of where birchtree has been and where it is going. Each phase adds a working slice; nothing is marked done without a passing test run behind it. No dates are promised for anything still open.

Status: everything through Phase 7.3 is built and green, including the October fix round. Next is Phase 8.

<!-- docs/logs/2026-10-20-fix-plan-verification.md:1-13; docs/logs/2026-10-20-phase-7-3-verification.md:36-52 -->

| Phase | What it means in plain words | Status |
|---|---|---|
| 0 | Write down the Mastodon 4.7.3 contract and set up the project | done (one checklist item, the route-by-route review, stays open) |
| 1 | Foundations: logging, health checks, safe shutdown, settings checks, IDs, file storage, first account from the command line | done |
| 2 | Sign-in and registration: app registration, login-and-approve pages, tokens, scopes, profiles, instance info | done |
| 3 | Social core: posts, replies, re-posts, likes, saves, pins, polls, quotes, follows, blocks, mutes, counters | done |
| 4 | Everyday use: uploads, timelines, notifications, lists, filters, search-over-local-posts, markers, scheduled posts | done |
| 4.1 | Gap closure: JSON request bodies, private lists hidden from home, home reads up to 1000 followed accounts | done |
| 5 | Live updates: one-way and two-way streams with shared visibility rules, heartbeats, and limits | done |
| 6 | Talking to other servers: discovery, profiles, follows, posts, edits, deletes, likes, boosts, polls, quote decisions, retries | done (content flows; thread-completion and the long tail below are a separate future phase) |
| 7 | Wider app compatibility: multi-account lookup, conversations, suggestions, announcements, donation placeholder, push notifications, companion-app endpoints | done |
| 7.1 | Media conversion: pictures to WebP, moving pictures to WebM, with startup checks and limits | done |
| 7.2 | Search engine: built-in index on by default, switchable to simple scan or off; private posts never indexed | done |
| 7.3 | Moderation and statistics: staff roles, reports, domain/email/IP rules, trend approvals, yearly summaries, usage measures | done |
| 8 | Performance qualification: measure speed and resource use on release builds for every surface above; decide on precomputed feeds only with numbers | next |

<!-- birchtree.md:408-565; src/config.rs:94-100; src/search/mod.rs:6-21; src/media/transcode.rs:1-22 -->

## Explicitly excluded or parked (untouched)

These are recorded decisions, not oversights. They stay as-is until an operator reopens them:

- **Fine-grained staff permissions (D35).** Today there are exactly two staff roles: manager (accounts and reports) and admin (everything). Per-action permission flags are future work.
- **Federation tail.** Account moves, remote emoji, relays and forwarded inboxes, follower-count syncing, note and featured collections, quote re-check workers, remote counters, poll update notifications, and live interop proofs each need their own work and their own register rows. The current "refuse what we do not understand" behavior stands until each piece lands.
- **Live settings reload.** Changing settings means restarting the server. That restart model stands.
- Also out of scope: translation service, `gifv` output, web admin dashboard, multi-server clustering, separate database or message queue, donation campaigns as real campaigns, and the old v1 search route (which does not exist in Mastodon 4.7.3).

<!-- src/api/mastodon/endpoints/admin_infra.rs:18; src/api/mastodon/auth.rs:71-127; src/config.rs:91-93; docs/TODOs.md:61-102 -->

## Non-goals

- No social-app source code in this project (the companion app is built separately and only its ready-made files are served).
- No server-rendered admin website; operators use the admin API and the command line.
- No extra infrastructure: no Postgres, no Redis, no separate message broker, no cluster mode until measurements demand a rethink.
- No fake successes: anything unbuilt answers an explicit error, never invented data.

<!-- birchtree.md:586-593 -->
