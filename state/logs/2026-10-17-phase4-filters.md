# 2026-10-17 — Phase 4: custom filters (FILTERS slice)

Branch/worktree: `phase4-filters` (on top of `b3e8551`, the Phase 4 foundation).

## What changed

- `src/application/filters.rs` — filter use cases:
  - v2 CRUD (`index`/`create`/`show`/`update`/`destroy`) with the exact
    Mastodon en.yml 422 messages (title blank/too long, context, action,
    expires-in, keyword blank/too long), owner-scoped 404s
    (`Record not found`), `expires_in=0`/blank → never, `FilterPatch` with
    `Some(None)` expiry clears and `keywords_attributes` translation
    (entries with an id inherit stored keyword/whole_word; id-less
    `_destroy` rows drop).
  - Keyword sub-resource (index/create/show/update/delete) validated through
    the parent filter.
  - Status rule sub-resource; `status_create` enforces status existence +
    fetchability via `policy::can_fetch` with `pair_state`.
  - v1 synthesized projection: one v1 "filter" = one keyword row
    (`v1_index`/`v1_create`/`v1_show`/`v1_update`/`v1_destroy`).
    `v1_create` makes filter + keyword in ONE `create_filter` call;
    `v1_update` answers the pinned multi-keyword 422 when a parent attribute
    (context/expires/irreversible) *would change* on a >1-keyword filter;
    `v1_destroy` deletes only the keyword.
  - `filter_results(store, viewer, statuses, context, now_ms)` — the
    `filtered`-array matcher: context-narrowed, non-expired filters; regex
    per keyword (escaped, case-insensitive, `\b` for whole_word) over
    source text + CW + poll option titles + media descriptions; status-id
    rules against `status.id` or `status.reblog_of_id`; entries via
    `dto::filter_result_json`, keyed by status id and duplicated under the
    reblogged id for wrapper rows.
- `src/api/mastodon/endpoints/filters.rs` — all 19 handlers (v1_index/
  create/show/update/destroy, v2_index/create/show/update/destroy,
  keywords_*, statuses_*); scopes `read|read:filters` / `write|write:filters`
  + `require_user()`; form decoding incl. an indexed
  `keywords_attributes[N][field]` parser and `expires_in` → 422
  ("Expires in is not a number"); responses via `dto::filter_json` /
  `dto::filter_v1_json` (keyword/status-rule standalone JSON mirrors the
  shapes `dto::filter_json` embeds).
- `tests/phase4_filters.rs` — 15 integration tests through the real router:
  exact v2 key sets, list/show round trips, update + expiry clear +
  keywords_attributes replace/destroy, every pinned 422 message, keyword and
  status sub-resources, v1 row projection, v1 atomic create, v1
  single-keyword update, multi-keyword guard (exact message), v1 destroy
  keeps the parent, foreign → 404 `Record not found`, scope 403s, and
  `filter_results` (payload shape, whole_word, expiry, context narrowing,
  status-id rules, poll options, reblog dual-keying).

No shared file was modified (ports/schema/dto/router/persistence/domain/
endpoints::mod untouched).

## Verified

- `cargo check --all-targets` — green, zero warnings.
- `cargo test` — green: lib 54, crash_recovery 3, persistence_conformance 18,
  phase2_api 13, phase2_oauth 13, phase3_interactions 8,
  phase3_relationships 13, phase3_status 13, **phase4_filters 15**,
  runtime 3, secret_handling 3 (154 total, 0 failures).

## Open questions / integration notes

- `filter_results` takes an explicit `now_ms` (expiry evaluation); the
  pinned signature in the slice brief omitted it.
- Timelines/notifications are NOT wired here (integrator's job): after
  loading a page, call `application::filters::filter_results` once with the
  page and copy the returned entries onto reblog wrappers (the function
  already keys reblog matches under both ids).
- `FilterStore::update_filter`'s returned bundle re-reads keywords from a
  pre-commit snapshot (adapter behavior, out of slice scope); the use case
  compensates with a fresh `filter_by_id` after the commit so PATCH
  responses carry the exact rule set.
- The v1 multi-keyword guard triggers on "would actually change" (a
  no-op `irreversible=0` on an already-`warn` filter passes); upstream
  Mastodon keys off parameter presence, so it would 422 there too. Pinned
  behavior follows the slice brief.
- `statuses_create` refuses statuses the viewer cannot fetch (404); upstream
  `Status.find` has no policy gate, so this is a stricter (defensible)
  reading pinned by the slice brief.
- Media descriptions are matched via `media_by_ids`, but no test uploads
  media (multipart) — that path is exercised only by construction.
