# Phase 4.1 — exit criteria checklist

Source: `birchtree.md` §7, Phase 4.1 ("Parity gap closure (before Phase 5)"),
and §8 item 11. Register rows: D10a, D10b, D11 (`docs/compatibility-profile.md`).
Evidence: `docs/logs/2026-10-17-phase-4-1-verification.md`. Work log:
`state/logs/2026-10-17-phase-4-1-parity-gap-closure.md`.

**Exit (§7):** every write endpoint has a JSON-and-form parity test with
identical results; an exclusive-list member is hidden from home, still visible
on the list timeline, and the viewer's own posts still show; the home cap is
1000 and the Phase 4 bounded-scan tests pass; the parity suite (§8 item 11)
covers the Phase 0–4.1 surfaces, or the evidence states which surfaces it does
not yet cover.

## JSON request bodies (D11)

- [x] `application/json` accepted on every REST write route that reads a body
  (`src/api/json_body.rs`; handlers read through `LenientForm` or `FormBody`).
  Flattening follows Rails' default parsing: `a[b]`, repeated `a[]`, indexed
  `a[N][field]` for arrays of objects, numbers and booleans as text, `null`
  absent. Evidence: `tests/phase4_1_json_parity.rs::json_and_form_bodies_answer_identically`
  (43 cases; each case also asserts its expected status), unit tests in
  `api::json_body`.
- [x] Body-less write routes accept and ignore a JSON body (four sampled
  routes in the table).
- [x] Malformed JSON answered as upstream's source gives: `400`
  `{"status":400,"error":"Bad Request"}` with a JSON `Accept`, otherwise an empty
  `text/html` `400`; refused before handlers and before authentication.
  Evidence: `malformed_json_*` tests in the same file. Derived from Rails
  8.1.3.1 and the v4.7.3 tree (see the verification log §2).
- [x] Form bodies and query strings unchanged (full suite green).
- [x] Excluded by design, each with a test or a note: multipart media uploads
  (untouched); `POST /oauth/authorize` stays form-only (`oauth_authorize_stays_form_only`).

## Exclusive lists on home (D10b)

- [x] A member of the viewer's `exclusive=true` list is left out of home, and
  is still shown on that list's timeline. Evidence:
  `tests/phase4_1_exclusive_home.rs::exclusive_member_is_left_out_of_home_and_shown_on_its_list`.
- [x] The viewer's own posts (and own reblogs) still show.
  `viewers_own_statuses_and_reblogs_are_never_left_out`.
- [x] A non-exclusive list hides nothing; clearing `exclusive` restores the
  member; another viewer's exclusive list hides nothing. Three further tests in
  the same file.
- [x] Rule implemented in the shared viewer-facts policy (`ViewerScope`,
  `src/application/timelines.rs`), matching `FeedManager#filter_from_home`.

## Home cap 1000 (D10a)

- [x] `HOME_AUTHOR_CAP` = 1000; `ACCOUNT_CACHE_CAP` = 2000 (≥1000 required);
  store `AUTHORS_CAP` = 1001. Evidence: `tests/phase4_1_home_cap.rs`
  (boundary: the 1000th follow's post is read; beyond: the 1001st is not).
- [x] A followed account's reblog of a stranger still shows at the cap
  (`a_reblog_of_a_stranger_still_shows_with_a_full_follow_list`).
- [x] Phase 4 bounded-scan limits hold at 1000 follows: `items_scanned` 2001,
  `read_txns` 5 (limits 5000 and 600), `home_scan_at_the_cap_stays_within_the_bounded_scan_limits`.
- [x] Dense follow sets: the merge defect found by measurement is fixed. The
  store's merge is now a lazy k-way merge, with a budget of 4000. The
  measurement is the ignored test `measure_dense_follow_sets_at_the_cap`
  (`PER_AUTHOR=n cargo test --test phase4_1_home_cap measure_dense -- --ignored --nocapture`).
  The table is in the verification log §4.

## Gate

- [x] `cargo check --all-targets`: zero warnings.
- [x] `cargo test`: 240 passed, 0 failed, 1 ignored (the measurement); 241 tests.
- [x] `bash scripts/smoke-all.sh`: `smoke-all: green`.
- [x] Mutation checks: account cache 1000, store author cap 500, and the
  exclusive rule disabled each make the intended tests fail
  (verification log §3–4).

## Parity (§8 item 11)

- [ ] **Not met: the parity harness is not built and was not run.** Neither a
  live pinned v4.7.3 comparison nor an automated observation suite exists. The
  parity claims in this phase are: JSON ≡ form (executed on birchtree);
  upstream's malformed-JSON answer, Doorkeeper's JSON handling, and
  FeedManager's exclusive-list rule (all source-derived from the pinned
  commit `8ff11c2b`, tag `v4.7.3`, and the Rails and Doorkeeper versions its
  lockfile pins). No Ruby, Postgres or Redis on the development machine, so the
  pinned instance cannot run here. Surfaces covered by source-derived evidence
  only: all Phase 4.1 surfaces. Surfaces with no parity observation at all:
  Phases 0–4 beyond what the Phase 4 suites assert against the contracts.
- [ ] Parity harness (`birchtree.md` §8 item 11) to be built in a later slice;
  Phase 5 streaming will need it for stream events.

## Known residuals (not register rows; see the state log)

- Malformed JSON to an unknown `/api/` path: 400 where upstream gives 404.
- JSON above 1 MiB: 413.
- `Vary: Authorization` not replicated on the malformed-JSON answer.
- `PATCH /api/v1/accounts/update_credentials` accepts multipart bodies
  (avatar/header file parts; shipped 2026-10-21, evidence
  `tests/avatar_upload.rs`). Urlencoded/JSON bodies keep the exact
  pre-multipart behavior (including the `415` for other content types).
- Invalid `replies_policy` on list create answers 422; upstream's answer for an
  invalid enum is not verified.
