# docs/TODOs.md

Status: Draft

Remaining work after the 2026-10-20 fix round (P0 privacy, P1 builds, P2
confirmed rows — all green, see `docs/fix-plan.md` and
`docs/logs/2026-10-20-fix-plan-verification.md`). Each item is honest
absence or a recorded gap — never a success-shaped stub. Items marked
**operator** need an operator decision before build; items marked
**phase** belong to a named future phase.

Conventions: `D##` = row in `docs/compatibility-profile.md`;
"manifest" = `docs/endpoint-manifest.md`.

## Needs an operator decision

- **Password-reset redemption flow.** The "forgot password" mail sends a
  real link, but the link target (`/auth/password/edit`-equivalent) has no
  page and no set-password port. Shape of the redemption HTTP flow is
  undecided.
- **Suggestion ranking source.** Suggestions rank honest local signals only
  (friends-of-friends, then most-followed fill). Confirm this is enough, or
  name a richer source (staff picks, similar-profiles, FASP, remote
  signals). Related: the profile directory is always on (upstream has a
  kill switch we did not build).
- **Site text settings.** Terms of service, privacy policy and extended
  description answer 404 — there is no authoring surface for the texts.
  Decide the settings/authoring shape (then the instance sub-resources serve
  them instead of 404ing).

## Media / profile follow-ups

- **Avatar/header upload in `update_credentials`.** `bot`, profile fields
  and attribution domains persist; `avatar`/`header` (+ descriptions) need
  the media pipeline (Phase 7.1 transcode + attachment ownership checks).
  Manifest notes it as a named follow-up.
- **Rule translations.** Server rules render `translations: {}` — no
  translation store exists.
- **Locale catalog for languages.** `GET /api/v1/instance/languages`
  renders `name` = code (e.g. `"fr"` instead of `"Français"`); upstream
  serves localized names from its ISO-639 catalog.

## Notifications / streaming follow-ups

- **Unproduced notification types (D51).** Ten types parse but are never
  emitted (poll updates, post updates, severed relationships, moderation
  warnings, annual reports, admin sign-ups/reports, quote updates,
  added-to-collection, collection updates). One register row per producer as
  the underlying features land.
- **Streaming announcement events.** Upstream emits `announcement`,
  `announcement.reaction` and `announcement.delete` on streams; we emit
  none (reads/dismissals/reactions are REST-only so far).
- **Store push-down for notification paging (deferred, not a bug).** The
  `account_id` fill loop is correct and cursor-stable; pushing the actor
  filter into the store page is a bounded perf optimization. Revisit only
  with measured scan pressure.
- **`Unsubscribe-URL` on pre-token push rows.** Subscriptions created
  before the fix round carry no token/URL and send no header (never
  fabricated). New subscriptions are complete.
- **`include_filtered` / `supported_types` honored in name only.** Both
  params parse, but filtered rows are never served into the v1 index and the
  `supported_types` fallback rendering is unbuilt — same observable result
  as upstream with no filtered content, but the knobs don't turn yet.

## Federation tail (own phase — D32 refuse-position stands)

- Account Move (migrated follows need an individual register row).
- Remote emoji ingest (`emojis` arrays omit remote emoji).
- Relays / inbox forwarding (delivery reachability claims).
- Follower sync and remote counters.
- Note / featured collections.
- Quote re-verify workers, remote counts, note collections.
- Poll `collection_update` notifications.
- Report forwarding as Flag to remote servers (Phase 6; local reports and
  the moderator queue are done).
- Live interop proofs (no live services in this environment — recorded,
  not chased).

## Collections surface

- `hide_collections` round-trips on the account row but there are no
  collections to hide (D52).
- Unbuilt endpoints (in manifest "not yet rowed" list): account `lists`
  sub-resource, `identity_proofs`, `email_subscriptions`, `in_collections`,
  top-level `collections` CRUD + items/revoke, `v1_alpha` namespace,
  `domain_blocks/preview`, `GET/PATCH /api/v1/profile`, instance
  `peers/search`, `GET /api/v2/notifications` family.
- `reblogged_by` lacks upstream's `distributable_visibility` scope (D52).

## Permissions / admin follow-ups

- **D35 roadmap:** fine-grained per-action permission flags on the upstream
  `Admin::BasePolicy` model (shipped: two-role split, moderator covers
  accounts+reports only).
- **Role serializer:** role ids are names (no numeric ids). Permissions are
  a real bitmask since the fix round.

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

## Accounts, auth, and moderation open items

- **MFA login unsupported.** No TOTP/WebAuthn second factor; password +
  (optional) approval only.
- **Registration `time_zone` / `date_of_birth` ignored; no min-age policy.**
  `locale` and invite text are captured; the other two fields are accepted
  and dropped.
- **First-party token-vs-session question (still open).** Whether
  first-party `/api/v1/*` calls use OAuth tokens (proposed default) or the
  browser session (endpoint-manifest web section).
- **Media caps policy.** Status length decided (500 default,
  `--max-status-chars` to 3000); the operator-facing media caps story
  beyond the upload size limit is unsettled.
- **IP-block client-IP verification.** Enforcement reads the trusted-proxy
  client IP; verify it sees the true client IP in all deployment shapes
  (no-access blocks depend on it).
- **Email-domain / canonical-email block enforcement vs confirmation
  state.** Registration-time enforcement is documented as needing email
  confirmation; confirm it works now that confirmation redemption is live,
  or record the remaining gap.

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

## Remote-media cache program (`docs/remote-media-cache-plan.md`, still a stub)

- Cache lifecycle: budgets + eviction (time expiry AND a global byte cap),
  per-class limits with a hard global cap (512 MiB proposed).
- Lazy fetch by default (metadata-first, fetch on first request) instead of
  ingest-time fetch.
- Durable-uploads isolation: separate remote-cache store + migration of
  existing `remote/*` keys; metadata model (source URL, blob key, class,
  size, fetch/last-access/expiry, ETag validators); no content-addressed
  sharing (frozen decision).
- Bounded ingest + periodic idempotent reconciler; accounting (total +
  per-class bytes incl. in-flight reservations); safe eviction (never touch
  local media); observability routing (interim log/health vs admin metrics).
- Acceptance tests (cap under concurrency, local survival, oversize abort,
  evicted re-fetch); normatives to
  `federation-contract.md`/`persistence-conformance.md`.
- Open questions: media-fetch signing (unsigned GET vs instance-actor
  dereference requirement); per-file ceiling reconciliation (40 MiB
  `MAX_REMOTE_MEDIA_BYTES` vs class caps); direct/streaming proxy delivery
  (disabled mode has no design).

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
