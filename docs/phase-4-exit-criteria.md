# Phase 4 — exit criteria checklist

Source: `birchtree.md` §7, Phase 4 ("Media, timelines, notifications and
client usability"). Contracts: `docs/endpoint-manifest.md` (timeline,
media, notification, list, filter, search, emoji and scheduled rows),
`docs/persistence-conformance.md`, `docs/application-ports.md`,
`docs/compatibility-profile.md`.

**Exit (§7):** real client media/post/timeline/notification workflows pass;
visibility parity is asserted across every read surface; large-page and
sparse-filter tests do not scan without bound; `202`/processing behavior and
all documented headers match.

## Media

- [x] Media v1 (`POST /api/v1/media`) and v2 (`POST /api/v2/media`) multipart
  upload, MIME/content-sniffing rejection (422), blank/missing file (422),
  description/focus round-trip, serving of stored bytes with the sniffed
  content type. Evidence: `tests/phase4_media.rs` (8), `scripts/smoke-phase4.sh`.
- [x] Processing lifecycle: v2 video/audio answers `202` with `url: null`,
  `GET /api/v1/media/:id` answers `206` while unprocessed (`url: null`) and
  `200` once the ProcessMedia job runs; the job is idempotent on replay.
  Evidence: `v2_mp4_upload_is_delayed_then_processed_by_the_job`,
  smoke-phase4 media steps.
- [x] Media attachment DTO nullability (`url`/`preview_url` null until
  processed; `meta`/`blurhash` when generated).

## Timelines

- [x] Home, public, local (`public?local=true`), hashtag and list reads with
  cursor pagination (`max_id`/`since_id`/`min_id`, exclusive) and viewer-aware
  filtering. Evidence: `tests/phase4_timelines.rs` (23).
- [x] Account statuses (the §7 "account" read) remain on the Phase 3 surface:
  `tests/phase3_status.rs::account_statuses_pinned_and_filters`.
- [x] Correct `Link` headers: `rel="next"` = `max_id` of the oldest item,
  `rel="prev"` = `min_id` of the newest item, present only for a non-empty
  page. Asserted with exact values in
  `cursors_limit_and_link_headers_match_the_contract`.
- [x] Documented headers: public and tag → `Cache-Control: max-age=15, public,
  stale-while-revalidate=30, stale-if-error=86400` for anonymous viewers;
  home, list and every authenticated response → `private, no-store`
  (`cache_control_follows_the_feed_and_the_viewer`). Deviation from the
  handoff's literal table is recorded in the manifest.
- [x] Timeline filtering rules tested: direct, private, mute, block (both
  directions), domain block, reblogs (`show_reblogs=false`, muted reblog
  targets), reply rules per feed, self-replies, orphan rejection, only-media,
  local/remote flags, tag normalization, list `replies_policy`
  (`list` / `followed` / `none`).
- [x] Errors match the pinned contract: list not owned or unparsable → `404
  {"error":"Record not found"}`; home without a token → `401`; app-only token
  on home/list → `422`; non-numeric cursor → `400 Invalid cursor value`;
  `limit=0` and non-numeric `limit` → empty page, no `Link`; `limit` clamped to
  40.

## Visibility parity

- [x] Visibility asserted on every timeline surface (home, public, tag, list):
  public-only feeds exclude unlisted/private/direct; direct reaches only named
  viewers; private needs a follow; blocked/muted/domain-blocked authors are
  excluded for the viewer only (anonymous viewers are unaffected).
- [~] **Cross-surface parity is not a single suite.** Status show/context/
  multi-fetch, account statuses and the relationship-gated interactions are
  covered by the Phase 3 privacy matrix (`tests/phase3_status.rs`,
  `tests/phase3_interactions.rs`); notifications by `tests/phase4_notifications.rs`.
  A dedicated parity matrix across all read surfaces is open (see below).

## Bounded scans and metrics

- [x] Sparse-filter scan is bounded: 300 candidate statuses, 280 of them from a
  muted author; the page of 20 is filled after walking 286 index entries with
  13 read transactions (asserted `items_scanned <= 5_000`, `read_txns <= 600`;
  measured numbers in `docs/logs/2026-10-17-phase-4-verification.md`).
- [x] Large-page bound: `limit` clamped to 40 (`limit=999` returns 40 rows).
- [x] Store scan caps are constants in `persistence/redb/timelines.rs`
  (public and hashtag 2 000 index entries; author merge 5 000 entries and 500
  authors); the application caps home follows and list members at 500 and
  per-request account facts at 500 (unknown accounts fail closed past it).
- [x] Store metrics (`read_txns`, `write_txns`, `items_scanned`) exposed and
  used by the bounded-scan test.
- [ ] **Wall-time measurement is not recorded.** The §7 wording asks for
  transaction/query counts *and* wall time; only counts are measured here.

## Notifications, markers, lists, filters, search, emoji, scheduled

- [x] Notifications: index with `types[]`/`exclude_types[]`, show, clear,
  dismiss, `unread_count`, `requests` (always `[]`, never fabricated), markers
  (`timeline[]`, `home`/`notifications` `last_read_id`). Evidence:
  `tests/phase4_notifications.rs` (4), smoke-phase4 notification/marker steps.
- [x] Grouping: the `ungrouped-<id>` fallback is the v1 profile (no server-side
  merging); documented in the manifest.
- [x] Lists and membership (`tests/phase4_lists.rs`, 5), including the
  follow-cascade.
- [x] Filters v1 and v2 (`tests/phase4_filters.rs`, 15). The `filtered`
  annotation is wired into status, notification and timeline reads
  (`home_timeline_carries_the_filtered_annotation`).
- [x] v2 search and custom emoji (`tests/phase4_search_emoji.rs`, 3).
- [x] Scheduled statuses (`tests/phase4_scheduled.rs`, 3).

## Client usability

- [x] `scripts/smoke-phase4.sh` drives the whole workflow against the live
  binary over HTTP — media (v1/v2, 202/206/200 processing, rejects), post with
  attachment, follow, home/public/tag/list timelines (incl. cache headers,
  404/401), notifications and markers, scheduled, search, emoji, Vary —
  and prints `SMOKE_PHASE4_OK`.
- [~] The script uses curl, not a third-party Mastodon client. Compatibility
  claims against real clients remain pending target-client testing.

## Gate (executed)

- [x] `cargo check --all-targets`: zero warnings.
- [x] `cargo test`: 219 passed, 0 failed (196 baseline + 23 timelines).
- [x] `bash scripts/smoke-phase4.sh`: `SMOKE_PHASE4_OK`.
- [x] `bash scripts/smoke-all.sh`: `smoke-all: green`.

Full pasted output: `docs/logs/2026-10-17-phase-4-verification.md`.

## Bugs found by the Phase 4 completion work (recorded for audit)

- **Hashtag index decode** (`persistence/redb/timelines.rs`): the id slice
  started at `tag.len() + 9` instead of `tag.len() + 1`, so any hashtag scan
  that reached an entry failed with `TryFromSliceError`. Latent until the tag
  timeline existed; fixed in `e7beac3`.
- **`smoke-phase4.sh` presence checks**: `jassert ... ""` cannot pass and its
  `exit 1` is not masked by `|| true`; replaced with real presence checks
  (`f5dd20a`).
- **`smoke-phase4.sh` media race**: the v2 video `GET` expected `206` but the
  250 ms ProcessMedia poll can finish first; the check now requires `206` ⇒
  null `url` and `200` ⇒ processed (`f5dd20a`).
- **Authenticated public/tag `Cache-Control`**: the handoff table says
  `public` for every public/tag response. That lets shared caches store
  viewer-specific bodies (`filtered`, flags), so authenticated responses send
  `private, no-store` instead. Anonymous responses keep the documented header.

## Open / deferred (explicit)

- **Cross-surface visibility parity matrix** (one suite spanning every read
  surface) — partially covered; open.
- **Wall-time measurement** of the page loaders — counts only; open.
- **Exclusive lists** (`exclusive=true` hides members from home) are stored but
  not applied to the home feed. Recorded as a manifest deviation.
- **Home reads the 500 most recent follows** (`HOME_AUTHOR_CAP`); follows
  beyond that are not shown. Recorded as a deviation.
- **Tag feed excludes reblog wrappers.** Wrappers carry no hashtags of their
  own, so the hashtag index never holds them. The handoff expected reblogs in
  the tag feed; this is an open question to settle against v4.7.3 before the
  Phase 4 sign-off is final. The test pins the current behavior and says so.
- **Remote statuses and domain blocks on remote accounts** cannot be created
  before federation (Phase 6). `remote=true` is therefore only tested as
  empty; the domain-block rule is exercised through the local domain.
- **Account-payload emoji arrays** remain `[]` → Phase 7.
- **Notification-policy filter pipeline** (notification requests) → later;
  `requests` is always `[]`.
- **Timelines: `206 Mastodon-Async-Refresh` regeneration** does not apply
  (fan-out-on-read never regenerates). Recorded as an architectural difference.
- **Pending doc decisions (not code gaps):** transcoding, search-engine
  choice, and admin-scope — to be settled in the separate docs re-scope
  handoff.
- Phase 0 route-inventory review against v4.7.3 remains open
  (`docs/phase-0-exit-criteria.md`).

## Status 2026-10-20 (freshness pass, worker W1)

Several "Open / deferred" bullets above are closed at HEAD and are
superseded, not rewritten: exclusive lists ARE applied to home
(`src/application/timelines.rs:49-105`, `ViewerScope::seed_exclusive` /
`on_exclusive_list`; Phase 4.1 evidence
`docs/logs/2026-10-17-phase-4-1-verification.md`); the home cap is 1000, not
500 (`HOME_AUTHOR_CAP` in `src/application/timelines.rs:26`,
`ACCOUNT_CACHE_CAP` at `:34`, store `AUTHORS_CAP` in
`src/persistence/redb/timelines.rs:39-41`); the tag-feed reblog question is
closed (D10c: wrappers carry no hashtags, excluded by design); the
transcoding, search-engine and admin-scope decisions are all settled AND
shipped (Phase 7.1 `src/jobs/transcode.rs:1-12`, Phase 7.2
`src/search/mod.rs:3-12` with `SearchMode` in `src/config.rs:94-97`,
admin scopes `src/api/oauth/scopes.rs:46-55`). Remote statuses, remote
media, and domain blocks on remote accounts now exist (Phase 6 content
flows; `src/application/federation_inbound.rs:100`,
`src/persistence/redb/federation_content.rs:455-505`). Still open as
stated: the cross-surface parity matrix, wall-time measurement, the
`Cache-Control` deviation record, account-payload emoji arrays (Phase 7
scope), the notification-policy pipeline, and the Phase 0 route-inventory
review.
