# Feature matrix

How birchtree compares with Mastodon 4.7.3, feature by feature, in plain words. "Built" means working today with passing tests; "planned" means not yet built (only Phase 8 items remain planned). Anything built but deliberately different is noted.

<!-- birchtree.md:4-6; docs/logs/2026-10-20-fix-plan-verification.md:1-13 -->

| Area | What it is | birchtree today |
|---|---|---|
| Sign-up and sign-in | Register apps and accounts, approve apps, log in, manage tokens and permissions | built (open/closed/approval modes; email delivery needs the operator to configure mail; login codes are single-use and short-lived) |
| Profiles and instance info | Names, bios, pictures, counters, server name and rules, preferences | built (profile pictures via upload are a named follow-up; rule translations and language display names are simplified) |
| Posts | Write, read, edit with history, delete, replies, threads, multi-fetch | built, including repeat-safe posting |
| Privacy levels | Public, unlisted (visible but not advertised), followers-only, direct (mentioned only) | built, under one shared rulebook |
| Quotes | Quote another post with the author's approval rules, list and revoke quotes | built locally and across servers |
| Following and people controls | Follow (with approval for locked accounts), block, block-a-domain, mute, notes, endorsements, lists | built |
| Likes, saves, pins | Like (favourite), save (bookmark), pin up to 5 | built, counters kept correct |
| Polls | Attach a poll, vote, hide totals until close, expiry | built locally and across servers |
| Media | Upload pictures/video/audio, descriptions, processing states, serve files | built; conversion to WebP/WebM runs as a background job |
| Timelines | Home, local, global, hashtag, list, account views with page cursors | built (home reads the 1000 most recent follows; private-list members hidden from home) |
| Notifications and markers | Who reacted to you, clear and dismiss, remember-my-place markers, scheduled posts | built (a few rare event types arrive with their features; requests list stays empty) |
| Filters and lists | Word/content filters, per-person lists | built |
| Search | Find posts, accounts, hashtags | built (built-in index on by default, switchable to scan or off; only public/unlisted posts indexed) |
| Live updates | Streams of new posts, edits, deletes, notifications | built over one-way and two-way connections, with shared visibility rules and limits |
| Finding other servers | "Where is this account?" lookup, "what is this server?" card, profiles, inboxes | built |
| Server-to-server posts and actions | Exchange posts, edits, deletes, replies, media, likes, boosts, polls, quote decisions, reports | built (content flows; moves, relays, forwarded inboxes, follower-count syncing, and collections are the parked tail) |
| Thread completion | Fill in missing parents and replies on remote conversations | parked as its own future phase |
| Conversations (direct-message threads) | Thread list, read/unread, mute a thread | built |
| Suggestions | "Who to follow" and profile directory | built (honest local ranking; richer sources are an operator decision) |
| Announcements | Server news, dismiss, react | built (authored through the admin API) |
| Donation banner | Optional campaign link | honest absence (404 unless the operator wires a campaign service) |
| Push notifications | Web push subscriptions and delivery | built (needs the operator's push keys; older rows without tokens send nothing rather than fake it) |
| Companion-app endpoints | Settings, embeds, push setup for the first-party app | built (bundle directory serves files when the operator installs it) |
| Staff roles | Manager (accounts and reports) and admin (everything), granted from the command line | built (finer per-action flags parked as D35) |
| Reports | File a report, staff queue, assign and resolve | built |
| Trends | Public tags/links/posts, staff approvals | built (only approved items shown publicly) |
| Yearly summaries | Per-account year in review, generate and mark-read | built |
| Statistics | Measures, dimensions, retention for staff | built (staff-wide read; operational metrics endpoint is Phase 8) |
| Speed and scale | Measured performance, precomputed feeds if numbers justify them | planned (Phase 8; current design reads on request with bounded scans) |

<!-- src/api/mastodon/router.rs:189-206,270-282,306-345,465-543,565-567; src/activitypub/http.rs:42-44; src/search/mod.rs:6-21; src/media/transcode.rs:1-22; src/api/mastodon/auth.rs:71-127; src/config.rs:94-100 -->

Notes on the fine print: resending the same request does not duplicate work; private and direct posts never leak through search, trends, timelines, streams, or server-to-server delivery; unbuilt routes answer an explicit error rather than invented data.

<!-- src/application/policy.rs; src/jobs/queue.rs -->
