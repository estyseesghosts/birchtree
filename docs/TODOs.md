# docs/TODOs.md

Status: Draft

Remaining work post fix round + storage overhaul (2026-10-21, `docs/database.md`)
+ gaps batch (`docs/gaps.md`, commit `722b024`). Database and gaps items
below are marked landed; what is left is verification debt, post-parity
programs, and deliberate exclusions.

Conventions: `D##` = row in `docs/compatibility-profile.md`;
"manifest" = `docs/endpoint-manifest.md`.

## Needs an operator decision

- **Suggestion ranking source.** Suggestions rank honest local signals only
  (friends-of-friends, then most-followed fill). Confirm this is enough, or
  name a richer source (staff picks, similar-profiles, FASP, remote
  signals). Related: the profile directory is always on (upstream has a
  kill switch we did not build).

## Media / profile follow-ups

- **Avatar/header upload in `update_credentials` (shipped 2026-10-21).**
  Multipart `avatar`/`header` file parts validate through the media path
  (still-image allowlist, 8 MB caps, decode check), stage under the blob
  store, and persist references on the account row in the same
  `update_account_profile` transaction; actor documents serve
  `icon`/`image` and a `profile_update` event fans out. Evidence
  `tests/avatar_upload.rs`.

## Notifications / streaming follow-ups

- **Producers landed (gaps G-B + G-D + G-C, D51).** All types except
  `severed_relationships` (no sever cascade exists — gaps-finish gap record)
  emit from real features; `added_to_collection`/`collection_update` flow
  from the `CollectionFanout` worker; announcement streaming events ride
  `user` streams. Evidence `tests/gaps_notifications.rs`,
  `tests/gaps_federation_tail.rs`, `tests/gaps_collections.rs`.
- **Store push-down for notification paging (deferred, not a bug).** The
  `account_id` fill loop is correct and cursor-stable; pushing the actor
  filter into the store page is a bounded perf optimization. Revisit only
  with measured scan pressure.

## Federation tail (landed gaps G-D; D32 refuse-position narrowed to truly-unknown)

- Account Move + migrated follows (D32a/D32b; `alsoKnownAs` link-back check open).
- Remote emoji ingest + status-array rendering (D32c; account payloads open).
- Relays / inbox forwarding (D32d).
- Follower sync and remote counters as untrusted hints (D32e).
- Note / featured collections (D32f).
- Quote re-verify workers, remote counts, note collections (D32g).
- Poll `collection_update` notifications (D32h; collection-side rows wait on G-C).
- Report forwarding as Flag to remote servers (D34; local queue → event → delivery verified).
- Live WebFinger resolve (D32j) and web embeds remote fetch/sanitize (D32k).
- Remaining: live interop proofs (no live services in this environment — recorded, not chased).

## Collections surface (landed gaps G-C)

- Top-level `collections` CRUD + `items` + `revoke`, `in_collections`,
  account `lists` sub-resource, `v1_alpha` namespace (minimal),
  `domain_blocks/preview`, `peers/search`, `GET /api/v2/notifications`
  family — all rowed in the manifest. `hide_collections` enforced
  everywhere (D52); `reblogged_by` carries the `distributable_visibility`
  scope. Evidence `tests/gaps_collections.rs`.

## Permissions / admin follow-ups

- **D35 landed (gaps G-E):** fine-grained per-action permission flags on
  the upstream `Admin::BasePolicy` model; moderator scope widens per flag
  as the remaining admin modules adopt `require_admin_action`; audit per
  write kept. Evidence `tests/gaps_auth_moderation.rs`.
- **Role serializer numeric ids + real bitmask (gaps G-E):** role ids are
  `0`/`1`/`2`, permissions the upstream bitmask string (`-1` gone; the
  `1049888` comment arithmetic corrected to `1049884`, verified against
  `user_role.rb` at `8ff11c2b`).

## Deliberately out of profile (not TODO, listed so nobody re-files them)

- Translation provider (`POST /api/v1/statuses/:id/translate`).
- `gifv` output (WebM/VP9 by decision D1–D4).
- Runtime config reload (restart model stands).
- Web UI beyond login/consent/first-party bundle; admin dashboard HTML;
  announcements HTML authoring (API surface built instead).
- FASP-sourced suggestions (`Mastodon-Async-Refresh` on v2).
- PostgreSQL/Redis/external broker, multi-node/cluster.
- Donation campaigns as real campaigns (honest 404 absence by decision).
- `GET /api/v1/oembed` (explicitly out of the initial profile).

## Accounts, auth, and moderation open items (landed gaps G-E unless noted)

- **MFA login (landed):** TOTP (+ hashed single-use backup codes) and
  WebAuthn (ES256, `none` + `packed` self-attestation) second factors;
  staged browser login gate (`otp_attempt`) with uniform refusals.
  Evidence `tests/gaps_auth_moderation.rs`. Follow-ups: TOTP envelope
  encryption at rest (no operator key exists yet); durable WebAuthn
  challenges (memory-scoped; `WEBAUTHN_CHALLENGES` table open);
  RS256/EdDSA authenticators + attestation chains (refused 422).
- **Registration `time_zone` / `date_of_birth` + min-age gate (landed):**
  captured on the credential row, validated (`validate_time_zone`,
  `check_date_of_birth`, min 16y), under-age refused 422. `minimum_age_years`
  config key landed (13–21); enforcement plumbing to the constant is open.
- **IP-block client-IP verification (landed):** true-client-IP matrix
  across proxy shapes pinned in `tests/gaps_auth_moderation.rs`.
- **Email-domain / canonical-email block enforcement vs confirmation
  state (landed):** signup-time matrix + confirmation reconciliation pinned
  in `tests/gaps_auth_moderation.rs`. Open: targeted
  `email_domain_block_for_domain` lookup (signup page is capped at 1000).
- **First-party token-vs-session question (still open).** Whether
  first-party `/api/v1/*` calls use OAuth tokens (proposed default) or the
  browser session (endpoint-manifest web section).
- **Media caps policy.** Status length decided (500 default,
  `--max-status-chars` to 3000); the operator-facing media caps story
  beyond the upload size limit is unsettled.

## Federation / remote-media fidelity

- **Remote media fidelity.** `REMOTE_MEDIA_SOURCE` stores each remote
  attachment's original URL but nothing surfaces it (`remote_url` is
  hardcoded null); no remote previews are generated (`preview_url` null
  for remote rows). Per-media blob keys already shard correctly.
- **Remote thread completion.** Ingest-time parent jobs (hop cap, fetch
  budget, never-promote-to-root), read-time descendant crawl with
  `Mastodon-Async-Refresh` + cooldown/dedup/caps, read-time ancestor
  completion for logged-in viewers (D5), signed-fetch instance key.
- **Live webfinger resolve.** Unknown remote accounts 404; resolving them
  over live webfinger is a federation follow-up.
- **Web embeds remote fetch/sanitize.** `GET /api/web/embeds/:id` answers
  404 for signed-in remote reads; the remote fetch/sanitize pass is unbuilt.

## Remote-cache cache program (`docs/remote-media-cache-plan.md`, storage overhaul landed 2026-10-21)

- Landed: two blob pools (`media_durable` + `media_cache`, tmp+rename, `0700`), `REMOTE_CACHE`/`CACHE_ACCOUNTING` tables (schema v17), `CacheEvict`/`ReconcileStorage` jobs (skeleton arms), metadata-first ingest shells, in-flight reservations + byte-LRU+TTL eviction (avatar reserve), stream-to-disk fetch (`tmp/{rand}.part`, mid-body abort), idempotent reconciler, `STORAGE_METRICS` + `/health` interim channel. Evidence `state/logs/2026-10-21-storage-overhaul.md`; suites `storage_two_pools` (3), `storage_cache_evict` (7), `storage_reconcile` (6).
- Remaining follow-ups (not regressions — behavior preserved):
  - Eager `FetchRemoteMedia` enqueue still fires at ingest; the flip to pure on-demand (enqueue only on cache miss at the media GET path) lands with the miss-path trigger.
  - `remote/*` bytes still live under the durable root; routing them through `BlobStores::pool(Cache)` + reconciler copy-and-repoint is open.
  - `upload_media` still buffers the request body (endpoint streams it); disk-staging the upload path awaits request-body streaming at the endpoint layer.
  - Durable `CacheStore` redb mirror is implemented; restart re-seeding of the process ledger from it is open.
  - Open questions carried (not absorbed): media-fetch signing (unsigned GET vs instance-actor dereference); per-file ceiling reconciliation (40 MiB vs class caps); disabled-mode direct/streaming proxy has no design.
- Normatives still to port: `federation-contract.md` / `persistence-conformance.md` cache rows (`REMOTE_CACHE`, `stage_remote_media`, bench-run5 media notes).

## Verification debt (tests, metrics, contract freeze)

- **Test suites:** API schema snapshots; request-parser suite
  (URL-encoded/multipart/JSON, nesting, bounds, Unicode); visibility policy
  matrix across reads/timelines/search/quotes/notifications/streaming;
  crash-test full matrix (during workers, joint durability, no-duplicate
  side effects); federation fixtures/interop suite; real-client automation;
  performance/abuse suite (high-degree graphs, sparse indexes, big bodies,
  upload exhaustion, slow hosts, retry storms). Test files live flat in
  `tests/` (the planned `tests/api_contract/` etc. subdirectories were
  never created).
- **Cross-surface visibility parity suite + wall-time measurement** (Phase 4
  exit carry-overs): one matrix spanning every read surface; loader
  wall-time, not just counts.
- **Operational metrics endpoint (Phase 8).** Admin analytics (`measures`/`
  dimensions`/`retention`) exist; a Prometheus-style operational endpoint
  does not.
- **Phase 8 perf qualification.** Release-build p50/p95/throughput/memory/
  contention/index/queue numbers on every surface incl. transcode, search,
  admin and fan-out-vs-materialized comparison; restart/kill/rebuild/replay
  robustness; latency/resource acceptance thresholds.
- **Contract freeze + source-audit review.** All contract docs still read
  `Draft` (operator approval pending); the endpoint-by-endpoint review
  against `config/routes/api.rb` was never recorded
  (`source-audit-v4.7.3.md: "None recorded yet"`).
- **Scheduled announcement publish job.** Scheduled rows stay drafts until
  manually published (no `PublishScheduledAnnouncement` job).
- **`indexable_statuses_page` port.** Historical unlisted rows join the
  search index only through the incremental job; the backfill underserves
  them until the port lands.
- **Tantivy search upgrade path.** Recorded future option behind the same
  backend port; not started.
- **Upload size cap conflated with multipart body limit.** `ApiState`
  carries no separate configured media cap.

## Post-parity programs (own phases, after the 98% bar)

Late-stage, config-gated, default-off. Absence verified 2026-10-20: zero
hits in `src/` for `aperture|antenna|ClipStore|AntennaStore|PageStore|
place_id|preferredLicense`, no `clips_enabled|antennas_enabled|
pages_enabled|aperture_mode` keys in `src/config.rs`, no such routes in the
router (`clip|page` hits are link-preview bodies and collection pagination
only). Full slice plans live in their own docs; the rows below are pointers,
not duplicates.

- **Aperture mode — photo-first Birchtree (`docs/aperture.md`, Draft).**
  Config switch (`aperture_mode`, default off): top-level posts require
  media (text-only → 422 local / silent drop on ingest), replies may be
  text-only, timelines photo-only, DMs exempt, poll-only top-levels
  dropped. Slices: A flag + write gate, B read path (timelines/context/
  search), C FEP-6757 licenses (minimal), D Places stub store +
  round-trip (no geo-search), E federation ingest policy + filter-name
  passthrough, F docs/gates/rollout. Parity must stay green with the
  flag off. Explicit non-goals: Pixelfed API imports, Stories, Groups,
  Portfolios, Discover namespace, server-side filter processing.
- **Clips module (`docs/misstodon.md`, plan; build after Batch Q lands).**
  MastoAPI-convention REST (`/api/v1/clips`, members, favourites),
  bookmarks become the private unrenamable system clip (two-way
  migration: forward into the clip, reverse via per-user export +
  bookmarks re-derived, user clips dormant), federation with pinetree
  (`OrderedCollection`, Add/Remove/Update/Delete, tombstones), owner-
  follower fan-out for Listed tiers, `ClipUpdated` notifications. Only
  the owner ever sees a private clip (404 to others).
- **Antennas module (`docs/misstodon.md`, plan).** Local-only (no peer
  implementation exists): keyword-group matching at read time, pull
  matched-statuses timeline (`/api/v1/antennas`), no streaming channel
  or push in v1. Explicitly not Mastodon filters (collect vs hide).
- **Pages module (`docs/misstodon.md`, plan).** `Article` + documented
  `source` extension (no new object type), visibility-gated reads (a
  documented divergence — pages are world-readable upstream), gated
  Article ingest mapping, origin fidelity (published/updated preserved),
  like/unlike + featured, `PageLiked` notifications, federation with
  pinetree (Create/Update/Delete fan-out, Like round-trip). Script
  stored, never executed, never federated.

## Performance follow-ups (deferred, not correctness)

- `PairState` extension (carry mute/domain facts in the pair row instead of
  extra store reads).
- Notification store push-down (see above).
