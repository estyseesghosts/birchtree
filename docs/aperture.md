Status: Draft

# Aperture mode — photo-first Birchtree (config-gated)

Goal: a config switch that turns Birchtree into a lightweight photo-first
service WITHOUT replicating Pixelfed's proprietary MastoAPI, Stories,
Groups, Collections, or Portfolios. Same Mastodon v4.7.3 routes; stricter
policy. Default off; Mastodon parity (§98% bar in
`docs/compatibility-profile.md`) must stay green with the flag off.

## Non-goals (explicit)

- No `api/v1.1`, `v1.2`, `v2.1`, `api/pixelfed/*`, `api/v0/groups/*`,
  cookie `api/compose/v0/*`, `api/direct/*`, `api/portfolio/*` imports.
- No Stories (expiry/views/reactions), Groups, Collections, Portfolios,
  full Discover surface, IG import, push-compare.
- Quote flow stays as-is (already implemented elsewhere); no Pixelfed
  `QuoteRequest` handshake work here.
- Filter pipeline is client-side; server does not process filters.

## Semantics (locked)

1. Top-level posts REQUIRE ≥1 image/video attachment. Text-only top-level
   → `422` (local API) / silent drop (federation ingest).
2. Replies (`in_reply_to_id` / `inReplyTo` present and resolvable) MAY be
   text-only. Replies render in `context`, never as timeline top-levels.
3. Timelines (`home/public/tag/list/link`) show only statuses with media
   when the flag is on. `context` shows full chains.
4. DMs (`direct` visibility) are EXEMPT from the photo rule.
5. Poll-only posts (Question, no media, top-level) are DROPPED under the
   flag (same as text). Documented deviation.
6. Licenses (FEP-6757 minimal) + Places (stub store + round-trip) ship;
   no geo-search, no license-sync-all, no Discover namespace. Discover =
   existing trends + tag timelines with `only_media` forced.
7. Existing text posts on flag-flip are grandfathered in storage but hidden
   from timelines (no backfill delete).

## Flag

`src/config.rs`: `aperture_mode: bool` (default `false`), plus
`aperture_require_alt_text: bool` (default `false`, optional stretch).
All behavior branches on `config.aperture_mode`; zero behavior change when
off (prove with gate suite).

---

## Phase A — Flag + write gate (no federation yet)

### Slice A1 — Config + plumbing
- Files: `src/config.rs`, `docs/configuration.md`,
  `docs/config-schema.json` (+ its pinning test).
- Instructions: add `aperture_mode` (bool, default false). Thread the
  resolved config into `application::statuses` validation entry point only.
  Do NOT touch timelines/federation here.
- Deliverable: flag parses from JSON config + CLI/env override; schema test
  green; docs row added.
- Test: `tests/aperture_a1_config.rs` (unique name; no shared test files).

### Slice A2 — Local write validation
- Files: `src/application/statuses.rs`,
  `src/api/mastodon/endpoints/statuses.rs` (thin error mapping only).
- Instructions: enforce — top-level (`in_reply_to_id` absent) with empty
  `media_ids` → `UnprocessableEntity` unless `visibility == direct`;
  reply with resolvable parent → allow text; unresolvable parent → existing
  error path unchanged. `PUT` edits may not strip the last attachment off a
  top-level post. Keep error shape Mastodon-compatible (`422` + JSON error).
- Deliverable: matrix green (top/reply/dm/edit × flag on/off).
- Test: `tests/aperture_a2_write.rs`.
- Exit: full suite passes with flag OFF (no regression) and new tests pass
  with flag ON.

## Phase B — Read path: timelines, context, search

### Slice B1 — Timeline filtering
- Files: `src/application/timelines.rs` (+ query type),
  timeline endpoint files under `src/api/mastodon/endpoints/`.
- Instructions: when flag on, force `has_media` predicate on
  `home/public/tag/list/link` queries regardless of client `only_media`.
  Do NOT change cursor semantics or `Link` headers.
- Deliverable: photo-only timelines under flag; byte-identical behavior
  with flag off.
- Test: `tests/aperture_b1_timelines.rs` (seed mixed photo/text/reply,
  assert timelines exclude text top-levels).

### Slice B2 — Context vs timeline split + search
- Files: `src/application/context.rs` (or wherever context is built),
  `src/application/search.rs` + `src/search/*`.
- Instructions: `GET context` returns full chains (text replies visible
  under their photo parent). `GET /api/v2/search` with flag on filters
  `statuses` to `has_media`. Grandfathered pre-flag text stays stored,
  excluded from timelines/search, reachable by direct `GET /api/v1/statuses/:id`.
- Deliverable + test: `tests/aperture_b2_context_search.rs`.
- Phase exit: B1+B2 green on/off; add deviation register row (aperture
  timeline scoping) in `docs/compatibility-profile.md` — PARENT EDITS ONLY
  (shared file; subagents propose snippet, do not edit).

## Phase C — Licenses (FEP-6757 minimal)

### Slice C1 — Storage + API param
- Files: `src/domain/*` (media/license types), `src/persistence/*`
  (schema bump N+1: `media_attachment.license NULL`, `account.default_license NULL`),
  `src/application/media.rs` + `src/application/accounts.rs`,
  `src/api/mastodon/endpoints/media.rs` + `statuses.rs`.
- Instructions: license enum mirrors Pixelfed ids
  (1,5,6,11–16, NULL=all-rights/unspecified). Compose accepts optional
  `license` per media (extension param, ignored when flag off). No
  sync-all, no validation beyond enum membership.
- Deliverable + test: `tests/aperture_c1_license.rs` (round-trip incl.
  NULL, reject out-of-enum with 422).
- Note: schema bump coordinated by PARENT (shared migration file).

### Slice C2 — Federation wire for licenses
- Files: `src/activitypub/wire.rs`, `src/activitypub/outgoing.rs`,
  `src/activitypub/activities.rs` (ingest mapping).
- Instructions: emit `license` on attachments + `preferredLicense` on actor
  (omit when NULL); parse both on ingest into C1 columns. No other
  behavior change.
- Test: `tests/aperture_c2_license_wire.rs` (golden JSON vs Pixelfed
  fixtures where available).
- Phase exit: local + wire round-trip green.

## Phase D — Places (stub)

- Files: new `src/domain/place.rs` (or extend existing), schema bump N+2
  (`place{id,name,lon,lat,country}` + `status.place_id NULL`),
  `src/application/statuses.rs` (accept optional `place_id`),
  `src/activitypub/wire.rs` + `outgoing.rs` (`location: Place{...}`
  round-trip).
- Instructions: store + return + federate `Place`; NO proximity/discover
  endpoints, NO validation beyond field lengths. Unknown `place_id` →
  existing 422 path.
- Deliverable + test: `tests/aperture_d1_places.rs`.
- Phase exit: stub green; geo-search explicitly deferred in doc.

## Phase E — Federation ingest policy + filters passthrough

### Slice E1 — Ingest gate
- Files: `src/application/federation_inbound.rs` (or `federation.rs`),
  `src/activitypub/activities.rs`, `src/federation/media.rs`.
- Instructions: flag on → `Create(Note)` with no valid
  `Document/Image/Video` attachment AND no resolvable `inReplyTo` → drop
  (log + counter, no error, no store). With `inReplyTo` → accept as reply
  (existing reply-park logic applies). `Update` same rule. Votes/likes/
  follows/DMs untouched. Keep D32 422 for genuinely unknown activity types.
- Test: `tests/aperture_e1_ingest.rs` (text top-level dropped, text reply
  kept, photo kept, poll-no-media dropped).
- Deliverable: interop note (Mastodon text posts won't surface under flag —
  by design).

### Slice E2 — Filter-name passthrough (optional, tiny)
- Files: `src/domain/*` media type (+ `filter_name NULL` col, schema N+3),
  `src/api/mastodon/endpoints/media.rs` (accept/store/return opaque string
  ≤64 chars), `src/activitypub/*` (preserve-or-drop documented; default:
  do NOT federate, store local only).
- Instructions: no processing, no validation beyond length. If contested,
  SKIP this slice entirely — client-side filters need zero server work.
- Test: `tests/aperture_e2_filtername.rs`.

## Phase F — Docs, gates, rollout

- Files (PARENT ONLY — shared): `docs/compatibility-profile.md` (D-rows),
  `docs/endpoint-manifest.md` (no new routes; behavior notes),
  `docs/federation-contract.md` (ingest-drop rule),
  `docs/phase-*-exit-criteria.md` checklists.
- Instructions: register deviations (422 top-level text, ingest drop,
  poll-no-media drop, timeline scoping, license/place extension params).
  Gate: `scripts/smoke-all.sh` (`cargo check --all-targets` + `cargo test`)
  green flag-off AND flag-on. Interop: two-instance run (both flags on,
  then mixed) + Pixelfed fixture thread (multi-photo Note + text reply).
- Deliverable: this doc marked Implemented-per-slice; dated log in
  `state/logs/YYYY-MM-DD-aperture.md` (new file per session, never rewrite).

---

## Subagent discipline (hard rules from AGENTS.md)

- Use `subagent` workers for parallel slices. Partition DISJOINT file sets
  per worker (see Files: lines above); no two workers touch the same file.
- Shared files (`docs/*`, schema/migrations, router, shared tests) are owned
  by the PARENT for merging — subagents PROPOSE patch snippets, never edit
  shared files directly.
- Subagents use UNIQUELY-NAMED test files (`tests/aperture_<slice>_*.rs`),
  leave changes uncommitted, never `git commit/stash/reset/checkout --`.
- `git status --short` before starting; respect the in-flight fix-plan
  worker's uncommitted files. Read-only recon must not write.
- Verify by execution: every slice lands with a runnable test + pasted
  output in the session log. No reasoning-only completion.
