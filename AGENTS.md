# AGENTS.md — birchtree (Mastodon 4.7.3-compatible SNS server)

Source of truth: `birchtree.md` (revised plan, pinned to Mastodon **v4.7.3**, commit `8ff11c2b`). If this file and `birchtree.md` disagree, `birchtree.md` wins; update this file.

## Layout

- `birchtree.md` — product/architecture plan and phased implementation (Phases 0–8).
- `docs/` — frozen and draft contracts (endpoint manifest, domain types, visibility policy, application ports, persistence conformance, federation/streaming contracts). See `docs/README.md`. Contract files carry a `Status:` line: `Draft` or `Frozen`.
- `docs/wiki/` — living overview (About, Current-State, Roadmap, Feature-Matrix, Home).
- `state/logs/` — dated append-only work logs. See `state/README.md`. One file per work session: `logs/YYYY-MM-DD-<topic>.md`.
- `docs/logs/` — append-only verification evidence (test runs, crash tests, interop runs).
- `pinetree`, `mastodon` — symlinks/reference checkouts outside this repo (`~/pinetree`, `~/fedifedifedi/mastodon`). Read only; used for API/streaming contract checks and patterns. Do not edit.
- `scripts/smoke-all.sh` — the full gate (`cargo check --all-targets` + `cargo test`); run it before claiming phase-level completion.

## Federation crate

ActivityPub federation is delegated to the `activitypub-federation-rust` crate rather than hand-rolled. Add it as a dependency when federation work (Phase 6) begins; until then the `federation/` module is a thin port/adaptation layer, not a custom ActivityPub implementation.

## Current phase: Phase 5 — streaming

Exit: `birchtree.md` §7 (Phase 5).
Phase 4 is complete and green — see `docs/phase-4-exit-criteria.md` (Phase 4
exit; open items listed there: cross-surface visibility parity suite,
wall-time measurement, exclusive lists on home, tag-feed reblogs question).
Phases 1–3 remain green (`docs/phase-1-exit-criteria.md`,
`docs/phase-2-exit-criteria.md`, `docs/phase-3-exit-criteria.md`). The
Phase 0 route-inventory review against the Mastodon v4.7.3 source remains open
(`docs/phase-0-exit-criteria.md`).

## State maintenance (required every work session)

1. Before starting: read `birchtree.md` §7 for your phase, `docs/README.md` for contract status, and the latest entry in `state/logs/`.
2. While working: update the relevant `docs/` contract in place; mark status `Draft` or `Frozen` at the top.
3. When finishing: append one dated log to `state/logs/YYYY-MM-DD-<topic>.md` recording what changed, what was verified, and open questions. Never rewrite past logs; supersede by reference.
4. Keep `docs/phase-0-exit-criteria.md` (or the current phase's checklist) checked off accurately. Do not claim exit criteria met without executed evidence.

## Architecture rules (from `birchtree.md` §§2–5)

- One modular monolith. Begin with one Cargo package; split into workspace crates only with a measurable reason.
- Dependency direction: `domain` ← `application` ← adapters. Domain knows neither HTTP DTOs nor ActivityPub JSON nor `redb`.
- REST and ActivityPub call the same application use cases. No duplicate business logic per adapter.
- One `redb` transaction commits state mutation + affected indexes/counters + idempotency/dedup records + bounded durable jobs atomically. Network I/O, media processing, notifications fan-out, federation delivery happen after commit.
- Mastodon-compatible decimal IDs: 64-bit timestamp-oriented IDs, serialized as decimal strings. Keep comparisons numeric internally; never order decimal IDs lexicographically.
- Contract first: pin endpoints, methods, encodings, scopes, response fields/nullability, status codes, headers, cursor semantics, streaming events, and unsupported behavior in a manifest backed by tests. No "success-shaped stubs": an unimplemented endpoint is explicitly out of profile and returns an appropriate error, not fabricated data.
- Federation input is untrusted: enforce size limits, timeouts, redirect limits, SSRF protections, signature verification.

## Verification

- Verify by execution: run tests/prototypes and paste or reference real output in the state log. Do not mark work done on reasoning alone.
- Compatibility claims require tests against real Mastodon v4.7.3 fixtures or target clients, not internal mocks.
- Both `cargo check --all-targets` and `cargo test` must pass on the release-relevant baseline before a phase exit is claimed.

## Subagent delegation (hard rule)

- Subagents share one worktree: concurrent subagents read and write the same files. The delegating agent must partition work so they cannot collide.
- Assign disjoint file sets; shared files (`docs/*` contracts, schema/migrations, router/ports, shared test files) are owned by the parent for merging — subagents propose patch snippets, they do not edit shared files directly.
- Subagents use uniquely-named test files, leave changes uncommitted, and never `git commit`, `git stash`, `git reset`, or `git checkout --` anything.
- Check `git status --short` before starting; respect uncommitted work by others; stage/commit only your own files when explicitly told to.
