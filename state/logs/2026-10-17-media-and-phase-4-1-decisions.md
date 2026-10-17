# 2026-10-17 — Media formats, register closure, Phase 4.1 handoff

Follows `state/logs/2026-10-17-parity-decisions.md`, which stays unchanged. Where this entry differs from it, this entry supersedes it. Documentation only. No code, tests or scripts changed in this session.

Phase reference: `birchtree.md` §5 (media transcoding), §7 (Phase 4.1, Phase 7.1), §8 (item 11), §9. Register: `docs/compatibility-profile.md`.

## Operator decisions

| Item | Decision | Recorded in |
|---|---|---|
| D3 video | **VP9 WebM** (Opus audio). Pass-through for content already VP9 within the defined size limit. | `birchtree.md` §7 Phase 7.1; register D3 (accepted) |
| D4 still images | **WebP** for every still image: JPEG, HEIC, HEIF, PNG, TIFF and any other. | Phase 7.1; register D4 (accepted) |
| GIF | **VP9 WebM**. | Phase 7.1; register D2 (accepted) |
| EXIF | The server **does not touch EXIF**. Privacy is handled on the client. | Phase 7.1 (metadata rule); register D4 |
| D9 home `206` | Not needed. Do not recreate it unless a client breaks. | Register D9 (accepted) |
| D10a home cap | **1000**. | Phase 4.1; register D10a (accepted) |
| D10b exclusive lists | Match upstream: hidden from home. | Phase 4.1; register D10b (decided) |

Earlier decisions, restated: D1 and D5 are intentional. D6 search is on by default, and admins can switch it off. The compatibility bar is 98% or better, with the core flows matching in full.

## Decisions I took (reversible; the operator should confirm or redirect)

These follow from the decisions above, or fill gaps the decisions left open. Each is written into the docs.

1. **Pass-through size limit = the upload cap** (`max_file_size`, 16 MB default). "Our defined file size limit" is read as this value.
2. **Pass-through conditions:** VP9 video in a WebM container, with Opus audio or no audio, within the limit. VP9 in another container is transcoded, not re-wrapped.
3. **Non-animated GIF is also converted to VP9 WebM** (typed `video`), because the decision says "GIFs become VP9 WebM". A one-frame WebM results.
4. **WebP passthrough:** an image already in WebP is stored unchanged.
5. **Preview:** the `small` preview is WebP too.
6. **Stored blobs:** a successful transcode replaces the original, which is deleted after the metadata commits. Pass-through keeps the original.
7. **Upload lifecycle:** v1 `POST /api/v1/media` transcodes inline and returns `200` with the final `url`, as upstream's v1 does. v2 returns `202` for transcoded types. A transcode over its limit ends `failed` (`422` on GET).
8. **Orientation:** not applied to pixels. The EXIF orientation tag is kept and clients apply it. This follows from "do not touch EXIF".
9. **"Core flows match in full"** is kept from my earlier proposal. The operator did not object.

## Upstream facts verified this session

- `lib/paperclip/transcoder.rb`: video targets MP4; WebM and QuickTime are converted to MP4; compatible video passes through without re-encoding; silent video gets `gifv`.
- `app/models/media_attachment.rb`: `file_styles` converts only HEIC, HEIF and AVIF to JPEG (`IMAGE_CONVERTIBLE_MIME_TYPES`). Other stills keep their format.
- `app/controllers/api/v1/timelines/home_controller.rb`: `206` with `Mastodon-Async-Refresh` (retry 5) while the home feed regenerates.
- `app/models/public_feed.rb` and `app/models/tag_feed.rb`: the tag feed joins `statuses_tags`. `app/services/reblog_service.rb` creates reblog rows without tags, so reblog wrappers never appear in the tag feed.
- `app/lib/feed_manager.rb`: `filter_from_home` returns `:skip_home` for members of the viewer's exclusive lists, except for the viewer's own statuses and except on list timelines.
- No `parameter_parsers` override in upstream `config/`, so Rails' default JSON parameter parsing applies.

## Code facts the next implementer needs (birchtree, read-only check)

- `src/api/forms.rs`: `parse` (line ~9), `get` (~64), `LenientForm` extractor (~117, `FromRequest` impl ~123). Every form-accepting write handler uses it.
- `src/application/timelines.rs`: `HOME_AUTHOR_CAP` (line ~26, currently 500); `LIST_MEMBER_CAP` (~28); `ACCOUNT_CACHE_CAP` (~32, currently 500, **also applies to home authors**, so a 1000 home cap without a higher account cap silently drops follows past 500).
- `src/config.rs`: `max_file_size` (line ~15; default `16 * 1024 * 1024`, line ~40).
- The Phase 4 bounded-scan tests (`tests/phase4_timelines.rs`, sparse-filter case) assert `items_scanned <= 5000` and `read_txns <= 600`.

## Documents changed

- `birchtree.md`: §3 (`transcode.rs` comment); §5 (transcoding paragraph: GIF, video, stills, metadata); §7 Phase 4.1 (rewritten: JSON bodies, exclusive lists, cap 1000, the account-cache dependency, exit gate); §7 Phase 7.1 (rewritten: decided rules, passthrough, metadata, lifecycle, startup validation, exit); §9 (open decisions and the decided list).
- `docs/compatibility-profile.md`: register rows D2, D3, D4, D9, D10a, D10b, D11 rewritten with decided statuses; "Decisions (2026-10-17)" replaces the recommendations block; Phase 4 timeline notes (1000 follows; tag feed matches upstream); planned surface (Phase 4.1; Phase 7.1); open decisions (media settings, search default closed).
- `docs/endpoint-manifest.md`: media deviation note (WebP, VP9, metadata); home cap 1000 (D10a); account-cap dependency; exclusive-list note (Phase 4.1); no-`206` note (D9).
- `docs/persistence-conformance.md`: home/list budget row (1000 authors).
- `docs/wiki/`: Roadmap (4.1 and 7.1 rows); Feature-Matrix (media and timelines rows); Current-State (home cap; 7.1 summary).

## Verification

- `grep` for stale wording (`500 most recent`, `target format`, `open sub-decision`, `recommended: accept`) across the plan, profile, manifest and wiki: no stale hits remain outside `docs/phase-4-exit-criteria.md`, which is off-limits.
- No code was changed, so the build and test results from `state/logs/2026-10-17-compat-bar-domain-blocks.md` still apply (221 passed, 0 failed at that point).

---

## Handoff: Phase 4.1 implementer

This section is the handoff prompt for the next implementer. The same text is given in the reply.

**Goal.** Close three parity gaps so an unmodified Mastodon client gets the same behaviour from birchtree as from Mastodon v4.7.3 on these surfaces: (1) JSON request bodies on every write endpoint, (2) exclusive lists hidden from home, (3) home follow cap raised to 1000 with the account cache raised to match. Phase 4.1 comes before Phase 5 (streaming).

**Read first, in this order.**
1. `AGENTS.md` (rules: state logs, contract updates, verification by execution, the gate).
2. `birchtree.md` §1 “The compatibility bar”, §7 Phase 4.1, §8 item 11.
3. `docs/compatibility-profile.md`: the bar, the register rows D10a, D10b, D10c, D11 and D9, and the planned surface.
4. `docs/endpoint-manifest.md`: the Phase 3 deviation list (JSON bodies), the Phase 4 timelines deviations, and the write-endpoint rows.
5. `state/logs/2026-10-17-media-and-phase-4-1-decisions.md` (this file) and `state/logs/2026-10-17-parity-decisions.md`.

**Starting state.** The working tree has uncommitted changes from the 2026-10-17 sessions: the domain-block correction (`src/`, `tests/`, `scripts/smoke-phase3.sh`) and the docs. Do not discard them. Ask the operator whether to commit them first. Last measured: `cargo check --all-targets` clean; `cargo test` 221 passed, 0 failed; `bash scripts/smoke-all.sh` green.

**Scope.**

1. **JSON request bodies (D11).**
   - Accept `Content-Type: application/json` (with or without `charset`) on every POST, PUT, PATCH and DELETE that currently takes a form body. Enumerate them from `src/api/mastodon/router.rs`. Multipart endpoints (`/api/v1/media`, `/api/v2/media`) keep multipart; leave them alone.
   - Flatten the parsed JSON into the existing `Vec<(String, String)>` model used by `src/api/forms.rs`: nested objects → `a[b]`; arrays → repeated `a[]`; numbers → decimal strings; booleans → `"true"`/`"false"`; `null` → key absent.
   - Form bodies and query strings must keep working unchanged.
   - Malformed JSON: find upstream's response (Rails parameter parsing, `Api::ErrorHandling` in `app/controllers/concerns/api/error_handling.rb`) and match its status and body. If the upstream response is unclear, stop and report it rather than guessing.
   - Test: for each write endpoint, the JSON request and the equivalent form request produce identical responses. Add a shared helper so the test is one table, not one test per route.
2. **Exclusive lists hidden from home (D10b).**
   - Match `FeedManager#filter_from_home` exactly. A status from an account that is a member of one of the viewer's lists with `exclusive=true` is left out of home. The viewer's own statuses are never left out. List timelines are unaffected.
   - Implement through the existing per-request viewer facts in `src/application/timelines.rs`, so the rule stays one policy function shared by the home use case.
   - Tests: a member is hidden from home and still shown on that list; the viewer's own post still shows; a non-exclusive list does not hide its members.
3. **Home follow cap 1000 (D10a).**
   - Set `HOME_AUTHOR_CAP` to 1000 (`src/application/timelines.rs`).
   - Raise `ACCOUNT_CACHE_CAP` to at least 1000 **in the same change**. Otherwise home authors past 500 fail closed and their posts are dropped. This is the trap; a test must cover it.
   - Keep the bounded-scan limits. The Phase 4 tests assert `items_scanned <= 5000` and `read_txns <= 600`. Run them. If a limit breaks with the new cap, do not weaken the test; keep the cap at 500 and report the measurement.
   - Update `docs/persistence-conformance.md` (the home/list budget row is already at 1000 authors) to match what you measure.
   - Test: a viewer following 1000 accounts sees a post from the 1000th follow; the 1001st is not read.

**Out of scope.** Phase 5 streaming. Transcoding (Phase 7.1). Search (Phase 7.2). Any `206` for the home feed (D9: accepted, do not add). EXIF handling (Phase 7.1 concern). Changes to the Phase 3 domain blocks beyond what is already done. Registration gaps (D12, Phase 2 items).

**Acceptance (exit gate for Phase 4.1).**
- Each write endpoint has a JSON-and-form parity test with identical responses.
- Exclusive-list rules pass: members hidden from home, shown on their list, own posts shown, non-exclusive lists unaffected.
- Home cap 1000 and account cache ≥ 1000; the boundary test passes; the Phase 4 bounded-scan tests pass.
- `cargo check --all-targets` has zero warnings. `cargo test` passes (baseline 221, expect more). `bash scripts/smoke-all.sh` ends `smoke-all: green`.
- Parity suite (`birchtree.md` §8 item 11): the harness does not exist yet. For these surfaces, either run the same requests against a pinned Mastodon v4.7.3 instance (if one can run on this machine) and record the comparison, or derive the expected behaviour from the pinned source and say in the evidence that it is source-derived. Do not claim parity without one of these.

**Evidence and records (required).**
- `docs/logs/<date>-phase-4-1-*.md`: pasted command output for check, test and smoke, and the parity evidence (or the source-derivation note).
- `state/logs/<date>-phase-4-1-*.md`: what changed, what was verified, open questions. Never edit earlier logs.
- Update the status rows: `docs/endpoint-manifest.md` (Phase 4 deviations: D10b fixed; D10a cap 1000; the Phase 3 JSON note), `docs/compatibility-profile.md` (D10b and D11 `implemented`, register D10a updated), `docs/wiki/Roadmap.md` (4.1 implemented), `docs/wiki/Feature-Matrix.md` and `docs/wiki/Current-State.md`.
- Do not edit `docs/phase-4-exit-criteria.md`, earlier `docs/logs/` entries, or `AGENTS.md`. `AGENTS.md` still names Phase 5 as current; tell the operator that line needs to change once 4.1 is done.

**Decisions already made. Do not reopen.** D1, D2, D5 intentional. D3 VP9 WebM with pass-through. D4 WebP, EXIF untouched. D6 search on by default with an admin off switch. D9 no `206`. D10a cap 1000. D10b hidden from home. D10c closed (matches upstream). D11 JSON bodies in this phase. The bar is 98% with the core flows matching in full.

**Escalate, do not decide.** Malformed-JSON response if upstream is unclear. A bounded-scan failure at cap 1000 that cannot be fixed inside this phase. Any upstream behaviour that contradicts the register.
