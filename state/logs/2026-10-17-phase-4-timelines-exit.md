# 2026-10-17 — Phase 4 completion: timelines slice + phase exit

Phase reference: `birchtree.md` §7 Phase 4 ("Media, timelines, notifications
and client usability"). Builds on the agent logs
`state/logs/2026-10-17-phase4-filters.md` and
`state/logs/2026-10-17-phase-4-media.md`; those are unchanged.

## Goal

Land the one missing Phase 4 slice (timelines: home, public, hashtag, list),
verify the whole phase by execution, and close the Phase 4 exit: the gate
green, the contract documented, and the exit checklist filled only with
executed evidence.

## What changed

Phase 4 timelines (the slice itself):

- `src/application/timelines.rs` — viewer policy and four use cases
  (`home`, `public`, `tag`, `list`). A request-scoped `ViewerScope` loads
  account facts in one `relationships` batch per account (cached, capped at
  500 accounts; overflow fails closed), records store errors and returns them
  after the scan. Filters: `HomeFilter`, `PublicFilter`, `TagFilter`,
  `ListFilter`.
- `src/api/mastodon/endpoints/timelines.rs` — the four handlers: Mastodon's
  `limit_param`, numeric exclusive cursors (400 on garbage), `Link` next/prev
  for non-empty pages, cache headers, `filtered` annotation for known viewers,
  exact 404/401/422 behavior.
- `tests/phase4_timelines.rs` — 23 tests (new suite).
- `docs/endpoint-manifest.md` — the four timeline rows flipped to
  `implemented (Phase 4)`; "Known Phase 4 timelines deviations" section added.
- `docs/phase-4-exit-criteria.md` (new), `docs/logs/2026-10-17-phase-4-verification.md`
  (new, pasted evidence).

Shared files, each in its own commit:

- `src/persistence/redb/timelines.rs` — hashtag index decode offset bug
  (`tag.len() + 9` → `tag.len() + 1`). Commit `e7beac3`.
- `scripts/smoke-phase4.sh` — two presence checks that could never pass, and a
  v2-video `206` expectation that raced the ProcessMedia worker (250 ms poll).
  Commit `f5dd20a`.

Docs updated by the completion commit: `docs/compatibility-profile.md`,
`docs/wiki/Roadmap.md`, `docs/wiki/Feature-Matrix.md`,
`docs/wiki/Current-State.md`, and `AGENTS.md` (current phase → Phase 5).

## Evidence (executed; full output in `docs/logs/2026-10-17-phase-4-verification.md`)

- `cargo check --all-targets` — zero warnings.
- `cargo test` — **219 passed, 0 failed** (196 baseline + 23 timelines).
- `bash scripts/smoke-phase4.sh` — `SMOKE_PHASE4_OK`.
- `bash scripts/smoke-all.sh` — `smoke-all: green`.
- Bounded scan: sparse-filter scan (300 candidates, 280 from a muted author)
  fills a page of 20 after **286 index entries** and **13 read transactions**
  (asserted `items_scanned <= 5000`, `read_txns <= 600`).

## Bugs found and fixed

1. Hashtag index decode read the id from the wrong offset; any hashtag scan
   reaching an entry failed with `TryFromSliceError`. Latent until the tag
   timeline existed.
2. `smoke-phase4.sh`: `jassert` with an empty expected value never passes and
   its `exit 1` ignores `|| true`. Replaced with real presence checks.
3. `smoke-phase4.sh`: the `206` expectation after a v2 video upload raced the
   worker. The check now requires `206` ⇒ null `url`, `200` ⇒ processed.
4. Authenticated public/tag responses: the plan's literal `public` header
   would let shared caches store viewer-specific bodies. Authenticated
   responses now send `private, no-store`; anonymous responses keep the
   documented header. Recorded as a deviation.

## Decisions taken without asking (reversible)

- The public feed runs the store with `include_replies=true` and lets the
  filter keep self-replies. The handoff's `include_replies=false` would have
  dropped every reply, contradicting its own "self-replies kept" rule.
- Own statuses always show in the viewer's own home feed, even when direct
  or replying to a stranger.
- `replies_policy=none` keeps only self-replies and replies to the viewer.
- Authenticated public/tag → `private, no-store` (see bug 4).

## Open questions and deferrals

- **Tag feed and reblogs.** Reblog wrappers carry no hashtags, so the tag feed
  never shows them. The handoff expected reblogs present. Needs a decision
  against v4.7.3 before the sign-off is final. The test pins the current
  behavior and says so.
- **Exclusive lists** are stored but not applied to home.
- **Home reads the 500 most recent follows.**
- **Cross-surface visibility parity** is covered per surface, not by one
  matrix suite. Open.
- **Wall-time** of the page loaders is not measured; counts only.
- **Remote** statuses and remote-account domain blocks wait for federation
  (Phase 6). The domain-block rule is exercised through the local domain only.
- Pending doc decisions in a separate docs re-scope handoff: transcoding,
  search-engine choice, admin scope. Not code gaps.
- Phase 0 route-inventory review against v4.7.3 is still open.

## Process notes

- The docs for compatibility profile, wiki and AGENTS were drafted by a
  subagent from a fact brief and reviewed by the main agent before commit.
- The smoke script is curl-based. It is not a third-party Mastodon client,
  so client compatibility is not yet verified.

## Next step

Phase 5 (streaming) per `AGENTS.md`. The docs re-scope handoff waits on this
completion commit.
