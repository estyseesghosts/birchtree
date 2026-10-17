# Phase 0 — exit criteria checklist

Source: `birchtree.md` §9, Phase 0.

**Exit:**
- [ ] Mastodon tag `v4.7.3` pinned (commit `8ff11c2b`, release 4.7.3, 2026-10-01); API v7 contract incl. quote additions recorded; reference-client versions listed.
- [ ] `config/routes/api.rb` and public OAuth routes inventoried in `docs/endpoint-manifest.md`; route table built for public/account/status/timeline/media/notification/poll/list/filter/search/preferences/scheduled-status/streaming endpoints; admin/moderation and optional/legacy APIs explicitly separated.
- [ ] For priority endpoints, controller → service → policy/model → serializer → worker path read; differences between public API docs and implementation recorded in `docs/source-audit-v4.7.3.md`.
- [ ] First contracts frozen/drafted: ID/cursor rules, account/status DTOs, visibility and interaction policies, application ports, durable job envelope, media lifecycle, error mapping (`docs/domain-contracts.md`, `docs/visibility-and-authorization.md`, `docs/application-ports.md`, `docs/persistence-conformance.md`).
- [ ] Versioned API compatibility profile defined in `docs/compatibility-profile.md`; exact scope reported (local only / federation subset / declared public API); full v4.7.3 compatibility not claimed by default.
- [ ] Crate initialized: `Cargo.toml`, `src/` layout per `birchtree.md` §3, `AGENTS.md`, `docs/`, `docs/wiki/`, `state/logs/` all present; `cargo check --all-targets` and `cargo test` baseline green.
- [ ] All unresolved ambiguities listed explicitly (none left implicit).

**Deliverables:**
- [ ] Route inventory reviewed against source (`docs/endpoint-manifest.md`).
- [ ] Domain/DTO glossary exists (`docs/domain-contracts.md`).
- [ ] Exact test fixtures exist (planned in `docs/testing.md`; first fixtures added in Phase 1).
- [ ] `cargo check`/`cargo test` baseline runs (see `state/logs/` for the green run).

Phase 0 is not exited until every box above is checked with executed evidence.
