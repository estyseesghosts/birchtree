# State log — 2026-10-16 project init

## Goal
Initialize the birchtree repository per `birchtree.md` Phase 0: write `AGENTS.md`, scaffold the Cargo crate (modular monolith, `domain` ← `application` ← adapters), create `docs/` contracts and `docs/wiki/`, set up append-only `state/logs/`, and note `activitypub-federation-rust` as the intended federation crate (Phase 6). Commit everything.

## What changed
- `AGENTS.md` — project rules, phase tracking, state-maintenance routine, federation crate note (`activitypub-federation-rust`, added at Phase 6).
- `README.md` — repo quickstart and references.
- `Cargo.toml` — `birchtree` 0.1.0, edition 2024; redb-only (no SQL/Redis/FDB), axum HTTP, tokio runtime; `thiserror` added. All deps resolvable from the local cargo registry cache.
- `docs/README.md`, `docs/phase-0-exit-criteria.md`, plus Draft contract skeletons: `architecture.md`, `source-audit-v4.7.3.md`, `compatibility-profile.md`, `endpoint-manifest.md`, `domain-contracts.md`, `visibility-and-authorization.md`, `application-ports.md`, `persistence-conformance.md`, `federation-contract.md` (notes delegation to `activitypub-federation-rust`), `streaming-contract.md`, `testing.md`.
- `docs/wiki/` — Home, About, Current-State (notes federation crate), Roadmap, Feature-Matrix.
- `src/` — full module scaffolding per `birchtree.md` §3: `main.rs`, `lib.rs`, `config.rs`, `observability.rs`, `domain/` (id, visibility, account, user, status, quote, relationship, follow, block, mute, favourite, bookmark, list, notification, poll, media, filter, error), `application/` (context, ports, stub use-case modules), `api/mastodon/` (router, auth, errors, request, pagination, rate_limits, dto, endpoints), `api/oauth/`, `activitypub/`, `federation/`, `streaming/`, `jobs/`, `media/`, `persistence/redb/`.
- `scripts/smoke-all.sh` — `cargo check --all-targets` + `cargo test`.

## Evidence
- `cargo check --all-targets` — green.
- `cargo test` — green (0 tests; baseline compiles and runs).

## Open questions
- Phase 0 contract review (route inventory vs `config/routes/api.rb`) begins next; exact test fixtures to be added in Phase 1.

## Next step
Review `config/routes/api.rb` against `docs/endpoint-manifest.md`; begin Phase 1 runtime/`redb`/ID scaffolding once Phase 0 contracts are frozen.
