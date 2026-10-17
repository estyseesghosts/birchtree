# Roadmap

Per `birchtree.md` §9, Phases 0–8.

| Phase | Focus | Status |
|---|---|---|
| 0 | Freeze source contracts; initialize crate | mostly done (route-inventory review open) |
| 1 | Runtime, `redb`, IDs, configuration, CLI bootstrap | implemented (gate green) |
| 2 | OAuth, registration, exact read APIs | implemented (gate green) |
| 3 | Local statuses, relationships, interaction semantics | implemented (gate green) |
| 4 | Media, timelines, notifications, client usability | implemented (gate green) |
| 5 | Streaming (SSE + WebSocket) | planned |
| 6 | ActivityPub federation | planned |
| 7 | Broader 4.7.3 public API parity | planned |
| 8 | Performance qualification; optional materialized feeds | planned |

Non-goals: PostgreSQL/Redis/separate broker; multi-node mode; full web UI/admin dashboard; React/Electron client.
