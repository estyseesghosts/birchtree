# Current state

This page says what the server does today. It is written for a smart reader with no programming background: what works, what is deliberately left out, and what is still planned. Short code pointers sit in hidden comments for the engineers; they are not needed to understand the page.

Status: late October 2026. Everything planned through Phase 7.3 is built and green. The October fix round (privacy, builds, confirmed rows) is finished and merged — it is history, not in-flight work. Next is Phase 8: measuring speed and robustness under load.

<!-- docs/logs/2026-10-20-fix-plan-verification.md:1-13; docs/logs/2026-10-20-phase-7-3-verification.md:36-52 -->

## What works today

**Accounts and sign-in.** Apps register themselves, people register accounts when the operator opens registration, and sign-in uses the standard Mastodon handshake including the extra-strong app proof (PKCE, a secret handshake that stops stolen codes from being reused). Scopes (permission labels like "read posts" vs "write posts") are enforced on every call. The first account is made from the command line, so you are never locked out.

<!-- src/api/mastodon/router.rs:189-206; src/api/oauth/scopes.rs:113; src/main.rs:193 -->

**Posts and conversations around them.** Write, read, edit (with history), delete, reply threads, re-posts, likes, saves, pins (up to 5), polls with expiry, and quote posts with the quoted author's approval rules (open to everyone, followers only, or nobody). Posting twice by accident creates only one post (repeat-safe keys kept for one hour per account).

<!-- src/application/statuses.rs:313,487; src/application/policy.rs:312 -->

**People controls.** Follow (locked accounts get approve/deny requests), unfollow, remove-a-follower, block (which also undoes follows both ways), block-a-whole-domain, mute (with timers), profile notes, endorsements (pinned profiles), follower/following lists, and per-account counters kept correct at the moment of change.

<!-- src/application/relationships/, src/application/policy.rs -->

**Who sees what.** One shared rulebook decides fetch, reply, re-post, like, quote, save, list membership, and delivery for public, unlisted (visible but not advertised), followers-only, and direct (mentioned people only) posts, including blocks and domain blocks. The October fix round closed three privacy gaps here (re-posts inheriting your default privacy, viewer-side block/mute/domain checks on reads and actions, and ungated undo of a re-post).

<!-- src/application/policy.rs:27-55,312-316; docs/fix-plan.md:54-63 -->

**Pictures, video, lists, filters, notifications.** Uploads (v1 answers at once, v2 answers "still working" then "ready"), descriptions and focus points, per-person lists (including private lists whose members are hidden from home), word/content filters, notifications with types, markers (remember-my-place), and scheduled posts.

<!-- src/api/mastodon/router.rs:465-543; src/media/blob_store.rs -->

**Search.** Status search runs on a built-in index that lives inside the server (on by default; the operator can switch to a simple recent-scan or turn status search off). Only public and unlisted posts are indexed — followers-only and direct posts never enter the index. Account and hashtag search use the database path in every mode.

<!-- src/search/mod.rs:6-21,36-63; src/config.rs:94-97 -->

**Media conversion.** Pictures become WebP and moving pictures become WebM video automatically after upload, by a background job. A missing or broken converter stops the server at startup rather than silently skipping work. The server never strips photo metadata.

<!-- src/media/transcode.rs:1-52; src/config.rs:98-117,383-406 -->

**Live updates.** Apps can subscribe to home, notifications, local/global, hashtag, list, and direct-message streams and get new posts, edits, deletes, and notifications as they happen, with heartbeats and sane limits. Blocking or muting someone retracts their posts from your live views.

<!-- src/streaming/hub.rs:60-63; src/streaming/http.rs:2,23-59; src/api/mastodon/router.rs:565-580 -->

**Talking to other servers.** Servers find each other, fetch each other's profiles, and exchange follows (accept/reject/undo), posts, edits, deletes, likes, boosts, blocks, polls, and quote decisions, with per-destination retries and quarantines so one broken server cannot hold up the rest.

<!-- src/activitypub/http.rs:1-4,42-44; src/federation/delivery.rs:13,21,55-61; src/federation/net.rs:5,102-175 -->

**Operator and moderation tools.** Two staff roles (manager and admin; the manager handles accounts and reports, the admin handles everything), granted from the command line. Reports queue, domain/email/IP rules, trend approvals (only approved tags, links, and posts are shown publicly), yearly per-account summaries, and usage statistics. Public instance pages (peers, rules, languages, activity, terms placeholder) are served from real data or answer an honest 404 when there is nothing to show.

<!-- src/api/mastodon/auth.rs:71-127; src/api/mastodon/router.rs:304-345; src/domain/user.rs:5; src/config.rs:89-93,138-165 -->

## Deliberately left out (not bugs)

- No `gifv` video type: moving pictures are served as WebM by decision.
- No translation service, no spell-out of remote emoji, no rich collections UI.
- Changing settings requires a restart; there is no live-reload mode.
- Fine-grained per-action staff permissions beyond the two roles are future work (recorded as D35).
- The long federation tail (account moves, relays and forwarded inboxes, follower-count syncing, note collections, remote counters) is its own future phase, not part of today.
- Announcements are authored through the admin API only (manual publish/unpublish); no web authoring page. Donation campaigns answer 404 unless the operator wires a campaign service. Password-reset links send real mail but the "choose a new password" page shape is undecided (operator decision).
- The web app beyond login/approve and the served companion-app files is out of scope, as is any admin web dashboard.

<!-- src/media/transcode.rs:6-21; src/config.rs:91-93; docs/TODOs.md:15-102; src/api/mastodon/endpoints/admin_infra.rs:18 -->

## Still planned, with no dates promised

- **Phase 8 — performance qualification.** Measure release builds (speed, memory, queue delays) on realistic data for every surface above, including conversion, search, admin lists, and live-update fan-out; compare the current read-on-request timeline design against an optional precomputed feed; publish numbers and thresholds. The operational metrics endpoint does not exist yet.
- **Smaller named follow-ups** (operator decisions where noted): avatar/header upload through the media pipeline, rule translations, language display names, unproduced notification/streaming event types, remote-media previews, live lookup of unknown remote accounts, and the remote-media cache program (budgets, eviction, accounting).

<!-- birchtree.md:558-565; docs/TODOs.md:15-200 -->

## How we know it works

Every phase shipped with executed evidence, not reasoning: warning-free checks, full test suites green, live HTTP smokes, and two-server interop runs against real Mastodon 4.7.3 where applicable. The detailed receipts live in `docs/logs/` (dated verification notes) and `state/logs/` (session notes). Anything unimplemented answers an explicit error, never a fake success.

<!-- docs/logs/2026-10-20-fix-plan-verification.md; docs/logs/2026-10-20-phase-7-3-verification.md -->
