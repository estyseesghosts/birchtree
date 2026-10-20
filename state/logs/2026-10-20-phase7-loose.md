# 2026-10-20 — Phase 7 loose ends + transcoding + search (HANDOFF_phase7-loose)

## Method

Five subagents in parallel with disjoint file ownership (no shared-file
edits, no cargo). Parent applied all shared-file patches, wrote the two
missing persistence modules (`redb/web.rs` from B's draft,
`redb/conversations_push.rs` new), implemented parent-owned pieces (E P4
backfill driver, P7 fold rendering; C P3 upload hook + worker threading;
A P-R worker threading; D P6 wiring), fixed every compile/test failure,
updated docs, extended the smoke script.

## What shipped

- **Conversations + push (A):** 4 conversation routes, read/unread,
  per-viewer delete (second delete 404s), conversation mutes with `muted`
  on the status DTO, `conversation` events on `direct` (closes D18), Web
  Push subscription CRUD (scope `push`, token-bound) + tickle-only
  delivery job (D42). VAPID: `vapid_public_key`/`vapid_private_key`
  config (base64url), server_key advertised, rotation = restart.
- **Web/* + odds (B):** 7 `/api/web/*` routes (session + double-submit
  CSRF cookie set at login), keystodon bundle at `/app` (404 unless
  configured), `timelines/link`, `apps/verify_credentials`, relation
  reads (blocks/mutes/favourites/bookmarks), familiar_followers (mutual
  follows), featured/followed tags + tag endpoints. 10 stale
  `planned` rows verified already-`implemented` (no flips needed).
  Remote embeds 404 (fetch/sanitize pass open).
- **Transcoding 7.1 (C):** ffmpeg job (GIF→WebM/VP9 `video`, video→WebM,
  stills→WebP, passthrough rules, `-map_metadata 0`, never `gifv`);
  v1 inline 200 / v2 deferred 202 → 206 → 200/422; startup toolchain
  validation; caps 120 s / 64 MiB / 1 GiB. Quality recorded in
  compatibility-profile. Default ON; `--no-transcode` opts out; CLI and
  harness paths never require ffmpeg (`Config::transcode_config`,
  enforced at serve startup only).
- **Search 7.2 (D):** `embedded` (default) / `database` / `off` switch
  (`--search-mode=`); in-process inverted index, zero new deps; public
  + unlisted scope; post-commit jobs + bounded restartable backfill
  (startup re-seed; cold/empty index degrades to the database scan).
  Tantivy recorded as future option.
- **Policy gaps (E):** D22 preference (`show` default / `collapse` fold
  via `filtered home-mention-fold warn` on home REST / `filter`
  upstream-like) — streams gate shares the REST home filter, so REST +
  streams agree; D23 follow `languages[]` + home filter + relationship
  rendering; D25 NFKC-subset `normalize_tag` (`café`=`cafe`) + writes
  normalized + backfill job; D26 creation gate (block/either-direction,
  any mute, domain block) with retroactive rule = leave.

## Parent-side fixes beyond subagent reports

- Fresh-thread unread: never-touched thread is unread when any message
  is foreign (upstream keeps unread until viewed), not
  `last_author != viewer`.
- `familiar_followers` intersects the target's *following* (mutual
  follows), not followers.
- New `FollowOptions.languages` / `UserSettingsUpdate.home_mention_policy`
  / `StatusViewFlags.muted` / `Relationship.languages` literals fixed in
  old tests; phase5 `direct`-silence test now expects `conversation`.
- TestServer disables transcoding (hermetic; no ffmpeg on dev boxes);
  smoke scripts pass `--no-transcode`; two `config.rs` unit tests
  updated for the new default (toolchain proof moved to serve startup).

## Verification

- `cargo check --all-targets`: warning-free.
- `cargo test`: 479 passed, 0 failed, 1 ignored (lib 138; binaries
  include `phase7_conversations_push` 9, `phase7_web_odds` 10,
  `phase7_1_transcode` 17, `phase7_2_search` 8, `phase7_policy_gaps` 16).
- `scripts/smoke-all.sh`: green (includes extended `smoke-phase7.sh`:
  conversations/mutes/push/stray rows/search/media/bundle-404 over HTTP).
- Evidence: `docs/logs/2026-10-20-phase-7-loose-verification.md`.

## Open items (not blockers)

- D P8 follow-up: `indexable_statuses_page` port so the backfill also
  covers historical unlisted rows (incremental path covers new ones).
- `TranscodeMedia` job kind reserved (lifecycle rides `ProcessMedia`).
- Encrypted push payloads (keys stored validated), remote oEmbed
  fetch/sanitize, TagBackfill chain re-seeds every boot (bounded full
  scan until a pass plans nothing).
- Admin track shares this tree (uncommitted); additive-only integration,
  no admin files touched.
