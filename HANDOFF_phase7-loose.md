# Handoff: Birchtree — Phase 7 loose ends + transcoding + search (NOT admin)

## Context (read first)

- Plan: `birchtree.md` §7 "Phase 7 (client-facing parity)", "Phase 7.1 (media transcoding)", "Phase 7.2 (search library)". Architecture rules: `AGENTS.md`.
- Contract rows: `docs/endpoint-manifest.md` — the sections "Conversations (Phase 7)", "Suggestions" (done, `[]`), "Announcements" (done, `[]`), "Web Push (Phase 7)", "`/api/web/*` (Phase 7)", plus stray Phase 7 rows elsewhere (`GET /api/v1/timelines/link`, `POST /api/v1/statuses/:id/mute|unmute` conversation mutes, `GET /api/v1/apps/verify_credentials`, relation collection reads `GET blocks/mutes/favourites/bookmarks`). The manifest is the source of truth — if a route isn't rowed there, don't build it. Note: 10 rows marked `planned` are already live in `router.rs` (accounts multi-fetch/lookup/search, reports, suggestions, announcements, donation campaigns); fix those stale statuses as part of this work.
- Reference: Mastodon v4.7.3 pinned tag (`8ff11c2b`), read-only. Behavior sources per endpoint are in the manifest's Notes column.
- Starting state: Phase 7 partially shipped (commit `0d402fd`); admin API is a separate handoff (`HANDOFF_admin-api.md`) — do not touch it. No ffmpeg/transcoding code exists (`src/media/processing.rs:4-14` documents the passthrough); search is a local-corpus scan (`GET /api/v2/search`, interim); no conversation domain records; no push machinery; no `/api/web/*`.
- Operator decisions (2026-10-20, recorded here — implement as stated):
  - **D22 (mention blocks/mutes on home): do NOT match upstream by default.** Upstream hides such statuses from home; we instead implement per-user options: (a) collapse behind a content-warning-style fold in feed, or (b) remove/filter entirely. Default = show (current behavior). Wire the choice through account preferences and apply it in both the REST home filter and the home stream gate (`src/application/timelines.rs`).
  - **D23 (follow language preferences): implement.** Store per-follow `languages`, apply on home like upstream's `filter_from_home`. Needs schema + API surface (follow create/update params) + migration.
  - **D25 (hashtag normalization): follow upstream.** Adopt NFKC folding (`Tag.normalize` parity) in `normalize_tag` (`src/application/statuses.rs:922`) plus a re-index/backfill of existing tag rows. `#café` and `#cafe` must be one tag.
  - **D26 (notification drop conditions): follow upstream.** `NotifyService` parity: never create notifications where the recipient blocks / is blocked by / mutes / domain-blocks the actor. Decide + document the retroactive question (delete pre-existing notifications from newly-blocked accounts or leave them).

## Objective

Close out everything in Phase 7 except the admin panel, land media transcoding (7.1) and a real search engine (7.2), and resolve D22/D23/D25/D26 per the decisions above — each with tests and manifest updates.

## Instructions (use subagents)

You MUST use subagents for parallel implementation. Suggested split (run in parallel; each owns its rows + tests + manifest updates):

- Subagent A (conversations + push): `GET/DELETE /api/v1/conversations*`, read/unread, conversation mutes (`mute`/`unmute` on statuses), `direct`-stream `conversation` events (closes D18). Web Push: subscription CRUD + delivery (needs VAPID keys from configuration). Confirm the push scope requirements against the manifest before building.
- Subagent B (web/* + odds and ends): `/api/web/*` session+CSRF routes, keystodon bundle serving (`web_client.rs` per plan §3), `GET /api/v1/timelines/link`, `GET /api/v1/apps/verify_credentials`, relation collection reads. Verify each against its manifest row; anything rowed `planned (Phase 7)` with no owner above belongs here.
- Subagent C (transcoding, Phase 7.1): ffmpeg-backed post-commit job per `birchtree.md` §5 "Media transcoding" — GIF→WebM/VP9 typed `video`, video→WebM/VP9+Opus (passthrough when already VP9 WebM within limit), stills→WebP (passthrough when already WebP), never strip EXIF, never produce `gifv` (D1–D4 stay as decided). Missing/non-working ffmpeg is a startup config error, never silent passthrough. Wall-time/output-size/memory caps per job. Must include the media-packer change (`url`/`preview_url`/`meta` per derived blob, `null` URL while pending) and a re-run of the media federation checks (bytes legitimately change now — coordinate with the harness note in `docs/federation-contract.md` "Media federation interop notes").
- Subagent D (search, Phase 7.2): evaluate embedded full-text library candidates for license/maintenance/fit (open decision in compatibility-profile), implement behind the `embedded`/`database`/`off` switch with `embedded` default (D6 stays: on-by-default is accepted). Keep the admin kill-switch working. Status search for authenticated callers; account search on the database path.
- Subagent E (policy gaps D22/D23/D25/D26): implement per decisions above. D22: preference-backed collapse-vs-filter in REST + streams (default show). D23: follow-languages storage + home filtering. D25: NFKC normalizer + tag backfill migration (bounded, restartable, idempotent). D26: drop conditions at notification creation + documented retroactive rule. Add compatibility-profile register updates for each (accepted/decided states).

Synthesize C+D+E before merging: transcoding changes served bytes (search indexing of media descriptions, if any, must use source text), and the D22 collapse rendering touches status DTOs both REST and streams consume.

## Questions to answer along the way

- Push: VAPID key configuration shape + rotation story?
- Search: which embedded library (license, maintenance, fit)? Record the evaluation, not just the winner.
- Transcoding quality settings (WebP/VP9) + `meta` for converted blobs — decide once, record in compatibility-profile.
- D26 retroactive rule: delete or keep pre-existing notifications from newly-blocked accounts?

## Implementation rules

- No "success-shaped stubs": unbuilt routes stay absent (404). Endpoints with no data yet (like today's `[]` suggestions) must be real reads over empty tables, not hardcoded literals — or stay absent.
- Keep architecture rules per AGENTS.md: domain has no HTTP/DB shapes; one txn = state + indexes/counters + jobs; no outbound HTTP or ffmpeg inside a request txn (transcode and push delivery are durable jobs, idempotent, bounded retry → quarantine).
- ffmpeg and the search index are derived state: crash-safe (orphaned staged blobs cleaned, index rebuildable), within the existing migration/conformance discipline.
- **Update `docs/endpoint-manifest.md` as you go: flip each shipped row to `implemented`, fix the 10 known-stale rows, correct any wrong pinned behavior.** A route that ships without its manifest row flipped does not count as done. Update `docs/compatibility-profile.md` (D22 new options row, D23/D25/D26 state changes, 7.1 quality settings).
- Append one dated log to `state/logs/YYYY-MM-DD-<topic>.md`; never rewrite past logs. Executed evidence in `docs/logs/`.
- Gate: `cargo check --all-targets` warning-free + full `cargo test` green (baseline 349/349), plus extended smoke coverage for conversations/push/transcode-job/search over HTTP.

## Deliverables

- Conversations (+mutes, +`direct` events), Web Push (subscriptions + delivery), `/api/web/*` + bundle serving, and every stray Phase 7 row — each tested or an explicit planned-gap entry.
- Transcoding pipeline (7.1) with packer changes + federation re-verification note.
- Search engine (7.2) with switch + evaluation record.
- D22/D23/D25/D26 implemented per the operator decisions above, with register updates.
- Manifest accurate to the code.

## Non-goals

- Admin API / roles / trends / annual reports belong to `HANDOFF_admin-api.md` — do not touch. Do not change the D1–D4 transcode targets or the D6 on-by-default search decision without operator sign-off. Do not run the §8 item 11 parity suite (separate milestone); just don't break its future inputs.
