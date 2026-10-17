# Current state

Status: 2026-10-16, project initialized, Phase 0 in progress.

## Implemented

- Repository layout per `birchtree.md` §3 (one Cargo package, `domain` ← `application` ← adapters)
- `AGENTS.md`, `docs/` contracts, `docs/wiki/`, `state/logs/`
- Crate scaffolding: all modules compile; no business logic yet
- `scripts/smoke-all.sh` gate: `cargo check --all-targets` + `cargo test` green
- Federation crate requirement noted: `activitypub-federation-rust` (to be added as a dependency at Phase 6)

## Not yet implemented

- All runtime behavior: IDs, `redb` storage, HTTP handlers, OAuth, federation, streaming, media
- Route implementations (all endpoints are stubs / out of profile until implemented)
- Tests beyond the compile-and-run baseline

## Known gaps

Everything in `birchtree.md` Phases 1–8. See `docs/phase-0-exit-criteria.md` for the Phase 0 gate.
