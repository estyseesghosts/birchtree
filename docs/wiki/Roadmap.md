# Roadmap

Per `birchtree.md` §7. Phases 0–5 (including 4.1) are implemented (Phase 0's route-inventory review is still open; the §8 item 11 parity harness is not built or run). Phase 6 is next. Every phase is measured against the compatibility bar: an unmodified Mastodon client has an identical experience to Mastodon v4.7.3 at 98% or better, and every deliberate difference is in the deviation register (`docs/compatibility-profile.md`).

| Phase | Focus | Status |
|---|---|---|
| 0 | Freeze source contracts; initialize crate | mostly done (route-inventory review open) |
| 1 | Runtime, `redb`, IDs, configuration, CLI bootstrap | implemented (gate green) |
| 2 | OAuth, registration, exact read APIs | implemented (gate green) |
| 3 | Local statuses, relationships, interaction semantics | implemented (gate green) |
| 4 | Media, timelines, notifications, client usability | implemented (gate green) |
| 4.1 | Parity gap closure: JSON request bodies on write endpoints; exclusive lists hidden from home; home follow cap 1000 | implemented (2026-10-17; parity evidence source-derived, see `docs/logs/2026-10-17-phase-4-1-verification.md`) |
| 5 | Streaming (SSE + WebSocket): declared streams, event envelopes, live policy, bounds, host redirect | implemented (2026-10-17; parity harness not run; direct and public:remote silent until Phases 7 and 6; see `docs/phase-5-exit-criteria.md`) |
| 6 | ActivityPub federation; remote thread completion; Flag | planned |
| 7 | Broader 4.7.3 public API parity: account multi-fetch, conversations, suggestions, announcements, donation campaigns, Web Push, first-party client (`/api/web/*`, keystodon bundle) | planned |
| 7.1 | Media transcoding: GIF and video → WebM/VP9 (video passthrough when already VP9 WebM within the size limit); still images → WebP; no `gifv`; the server never strips EXIF | planned |
| 7.2 | Search: embedded full-text library behind `GET /api/v2/search`; optional (`embedded`, `database`, `off`) | planned |
| 7.3 | Administration, moderation, trends and analytics: admin API, reports, trends, annual reports, admin metrics | planned; needs a role model and the Phase 6 enforcement points |
| 8 | Performance qualification; optional materialized feeds; covers every surface above | planned |

Phase 7.1 and 7.2 have no dependency on Phases 5 or 6 and may be scheduled earlier. Phase 7.3 depends on Phase 6 for server-level domain blocks and reports forwarding.

keystodon, the frontend, is developed in parallel in a separate project. birchtree serves its built bundle as the first-party client, and keystodon also works as a third-party OAuth client against any host. It is the planned interoperability target for both modes.

Non-goals: PostgreSQL/Redis/separate broker; multi-node mode; frontend source code in this repository (birchtree serves keystodon's bundle); a server-rendered admin dashboard (the admin API is in profile); React/Electron client.
