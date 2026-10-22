# birchtree wiki

Status: living documentation, updated 2026-10-20 (Phases 0–7.3 implemented including the fix round; Phase 8 next).

The authoritative sources are `birchtree.md` (product/architecture plan, pinned to Mastodon v4.7.3) and the contracts in `docs/`; this wiki is the readable overview. If they disagree, the plan and contracts win.

| Page | What it covers |
|---|---|
| [About birchtree](About.md) | What birchtree is, how it is built, how to run it |
| [Architecture](Architecture.md) | Process/crate graph, module tree, data flows, dependency direction, IDs, mutation protocol, jobs, storage, timelines, config |
| [Federation](Federation.md) | Crate split, discovery, signatures, safe HTTP, ingress/egress, activity table, thread completion, remote media, self/Mastodon/Misskey/generic interop |
| [Client API](Client-API.md) | OAuth, endpoint groups, coverage vs 404s, DTO notes, parsing/pagination/errors/rate limits, divergences, masto-fe breaks |
| [Administration](Administration.md) | Roles, config, JSON schema, moderation endpoints, stored-vs-enforced, divergences, operator runbook |
| [Operations](Operations.md) | Production requirements, ports/paths, data-dir, backups, logging, caps/tuning, failure modes, security |
| [Glossary](Glossary.md) | Every term a newcomer needs: accounts, visibility, quotes, policies, inboxes, streams, filters, D-codes |
| [Current state](Current-State.md) | Repo layout, implemented surface, tests, known gaps |
| [Roadmap](Roadmap.md) | Phases 0–8 (with sub-phases 7.1–7.3): done, in progress, planned; non-goals |
| [Feature matrix](Feature-Matrix.md) | birchtree vs Mastodon 4.7.3, feature by feature |

Related documents: `README.md` (repo-root quickstart), `docs/README.md` (contract index), `docs/phase-4-exit-criteria.md` (Phase 4 gate), `docs/phase-4-1-exit-criteria.md` (Phase 4.1 gate; parity harness not yet built), `docs/phase-5-exit-criteria.md` (Phase 5 gate, streaming; open escalations), `docs/streaming-contract.md` (streaming contract, Draft), `docs/phase-0-exit-criteria.md` (open route-inventory review), `state/logs/` (dated work logs), `AGENTS.md` (rules for agents working in this repo).
