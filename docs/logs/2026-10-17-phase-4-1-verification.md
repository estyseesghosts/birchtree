# 2026-10-17 — Phase 4.1 verification (parity gap closure)

Append-only evidence for `state/logs/2026-10-17-phase-4-1-parity-gap-closure.md`. Scope: D11 (JSON request bodies), D10b (exclusive lists hidden from home), D10a (home cap 1000, account cache). Nothing is committed; the working tree still carries the 2026-10-17 domain-block changes from the starting state.

## 1. Full gate

Command: `bash scripts/smoke-all.sh > /tmp/smoke-all-3.log 2>&1` (exit 0; this is the final run, after the last code change). The last lines:

```
==> scripts/smoke-phase4.sh
...
==> Vary header on API responses
SMOKE_PHASE4_OK
...
smoke-all: green
```

Each phase smoke passed: `SMOKE_OK` (phase 1), `SMOKE_PHASE2_OK`, `SMOKE_PHASE3_OK`, `SMOKE_PHASE4_OK`.

`cargo check --all-targets` (inside the gate) finished with no warning lines (`grep -c "^warning" = 0`):

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.16s
```

`cargo test` per binary (final gate run, verbatim `test result` lines, paths trimmed):

```
src/lib.rs                              test result: ok. 76 passed; 0 failed; 0 ignored
src/main.rs                             test result: ok. 0 passed; 0 failed; 0 ignored
tests/crash_recovery.rs                 test result: ok. 3 passed; 0 failed; 0 ignored
tests/persistence_conformance.rs        test result: ok. 18 passed; 0 failed; 0 ignored
tests/phase2_api.rs                     test result: ok. 13 passed; 0 failed; 0 ignored
tests/phase2_oauth.rs                   test result: ok. 13 passed; 0 failed; 0 ignored
tests/phase3_interactions.rs            test result: ok. 8 passed; 0 failed; 0 ignored
tests/phase3_relationships.rs           test result: ok. 14 passed; 0 failed; 0 ignored
tests/phase3_status.rs                  test result: ok. 13 passed; 0 failed; 0 ignored
tests/phase4_1_exclusive_home.rs        test result: ok. 5 passed; 0 failed; 0 ignored
tests/phase4_1_home_cap.rs              test result: ok. 4 passed; 0 failed; 1 ignored
tests/phase4_1_json_parity.rs           test result: ok. 6 passed; 0 failed; 0 ignored
tests/phase4_filters.rs                 test result: ok. 15 passed; 0 failed; 0 ignored
tests/phase4_lists.rs                   test result: ok. 5 passed; 0 failed; 0 ignored
tests/phase4_media.rs                   test result: ok. 8 passed; 0 failed; 0 ignored
tests/phase4_notifications.rs           test result: ok. 4 passed; 0 failed; 0 ignored
tests/phase4_scheduled.rs               test result: ok. 3 passed; 0 failed; 0 ignored
tests/phase4_search_emoji.rs            test result: ok. 3 passed; 0 failed; 0 ignored
tests/phase4_timelines.rs               test result: ok. 23 passed; 0 failed; 0 ignored
tests/runtime.rs                        test result: ok. 3 passed; 0 failed; 0 ignored
tests/secret_handling.rs                test result: ok. 3 passed; 0 failed; 0 ignored
```

Totals: 240 passed, 0 failed, 1 ignored (the dense-follow measurement, §4). 241 tests in total against the 221 baseline: +4 unit tests (`api::json_body`), +6 JSON parity, +5 exclusive-list, +4 home-cap, +1 ignored measurement. Zero warning lines in the gate log.

## 2. JSON bodies (D11) — parity evidence

**Internal parity (JSON ≡ form), executed.** `tests/phase4_1_json_parity.rs::json_and_form_bodies_answer_identically` runs 43 cases. Each case builds the same fixtures on two fresh in-process servers. One server gets the form body, the other the JSON equivalent, and both responses must match on status, `Content-Type` and body. Every case also asserts its expected status, so a pair of identical failures cannot pass. Output: `test json_and_form_bodies_answer_identically ... ok` (`6 passed` for the binary, with the malformed-JSON and exclusion tests below).

Normalization, applied before comparing: tokens, secrets and `client_id` are masked by key (they are random per server); numeric `*_at` values are masked (OAuth `created_at` can straddle a second); media file paths are cut at `/system/media_attachments/files/` (their hash segments are random); every digit run becomes `#`. No other masking.

Coverage (43 cases, grouped): apps create; OAuth token (client credentials) and revoke; accounts register, update_credentials (nested `source[...]`), follow with flags, mute with `duration` as a number, note, domain block create and delete; statuses create (incl. `language`, `sensitive` boolean, `media_ids[]`, nested `poll[...]` with numeric `expires_in`), create with no text (422 both), update PUT and PATCH, delete, interaction policy PUT and PATCH, reblog and favourite (body-less); poll vote (`choices[]` numeric); media update PUT and PATCH, media delete; list create (with `replies_policy` and `exclusive`), list update PUT and PATCH, add and remove members (`account_ids[]`); v1 filter create, update, delete; v2 filter create with `keywords_attributes` as an array of objects (`keywords_attributes[N][keyword]`), update, keyword create and update, filter status create and delete; markers create (`home[last_read_id]`); scheduled status update and delete; unfollow and notifications clear (body-less, JSON ignored).

Every write route in `router.rs` that reads a body is in the table, except the two exclusions below.

**Excluded from the table, by design:**
- `POST /api/v1/media`, `POST /api/v2/media`: multipart; untouched (the brief's "leave the multipart media endpoints alone").
- `POST /oauth/authorize`: browser consent form. Upstream guards it with `protect_from_forgery` (CSRF token) through Doorkeeper's authorizations controller, so a JSON client cannot submit it. Kept form-only.

**Exclusion, executed:** `oauth_authorize_stays_form_only` (a valid JSON body to `POST /oauth/authorize` answers 415).

**Malformed JSON, executed:** `malformed_json_with_json_accept_matches_upstream` (400 `application/json; charset=utf-8`, body `{"status":400,"error":"Bad Request"}`), `malformed_json_without_json_accept_is_an_empty_html_400` (400 `text/html; charset=utf-8`, empty), `malformed_json_fails_before_the_handler_and_before_auth` (a body-less route with a valid token, and a route with no token, both 400), `empty_json_body_is_no_parameters` (empty JSON body equals an empty form: 422).

**Upstream derivation for the malformed-JSON answer (source-derived, not run):**
- Pinned commit `8ff11c2b` (tag `v4.7.3`) in `~/fedifedifedi/mastodon`. `Gemfile.lock` pins `rails (8.1.3.1)`.
- Rails 8.1.3.1 (raw.githubusercontent.com, tag `v8.1.3.1`):
  - `actionpack/lib/action_dispatch/http/parameters.rb`: the JSON parser raises `ParseError` on bad JSON; a zero-length body skips parsing; non-object JSON is wrapped under `_json`.
  - `actionpack/lib/action_dispatch/http/mime_negotiation.rb`: `formats` falls back to `Accept` when params cannot be read (`params_readable?` is false on `ParseError`), with `BROWSER_LIKE_ACCEPTS`.
  - `actionpack/lib/action_dispatch/middleware/show_exceptions.rb`: an `X-Cascade: pass` response becomes an empty response with the status (`pass_response`).
  - `actionpack/lib/action_dispatch/middleware/public_exceptions.rb`: a JSON-negotiated request gets `{status, error}`; otherwise `public/<status>.html`, or a 404 with `X-Cascade: pass` when that file is missing.
- Upstream tree at `8ff11c2b`: `app/controllers/concerns/api/error_handling.rb` and `error_responses.rb` map no `ParseError`; `public/` contains `500.html` only (no `400.html`); `config/` sets no `parameter_parsers` override.
- Consequence: JSON `Accept` → JSON body; anything else → empty `text/html` 400. Implemented in `src/api/json_body.rs::malformed`.
- OAuth token and revoke take JSON too. Doorkeeper 5.9.9 (locked at `8ff11c2b`, `Gemfile.lock` line 214; raw source at tag `v5.9.9`): `Doorkeeper::TokensController` reads `params[:grant_type]` and `params["token"]`, and `Doorkeeper::OAuth::Client::Credentials.from_params` reads `request.parameters`, which is Rails' JSON-aware parameter set. Upstream `OAuth::TokensController < Doorkeeper::TokensController` adds no parameter handling.

**Parity not run against a live v4.7.3.** `which ruby bundle psql redis-server` finds nothing on this machine, so the pinned instance cannot run here. The §8 item 11 harness does not exist yet. Per the brief, the evidence above is source-derived for upstream and executed for birchtree's JSON ≡ form equivalence.

## 3. Exclusive lists on home (D10b) — executed

`tests/phase4_1_exclusive_home.rs` (HTTP, through the router):
- `exclusive_member_is_left_out_of_home_and_shown_on_its_list`: exclusive member's post absent from home, present on the viewer's list timeline (`[bob_post]`), non-member's post still on home.
- `viewers_own_statuses_and_reblogs_are_never_left_out`: the viewer's own post and the viewer's reblog of the member's post both on home; the member's original absent.
- `non_exclusive_list_hides_nothing`.
- `clearing_exclusive_restores_the_member_on_home`: `PUT exclusive=false` brings the member back.
- `only_the_viewers_own_exclusive_lists_count`: another user's exclusive list does not hide the member.

Upstream rule implemented: `FeedManager#filter_from_home` (`app/lib/feed_manager.rb` 448–479 at `8ff11c2b`): `return if receiver_id == status.account_id`, then `return :skip_home if timeline_type != :list && exclusive_list_users[status.account_id]`; `crutches_exclusive_list_users` (649–656) is `ListAccount` rows of the receiver's `exclusive: true` lists.

Mutation check (exclusive rule disabled with `if false && ...`): `exclusive_member_is_left_out_of_home_and_shown_on_its_list`, `viewers_own_statuses_and_reblogs_are_never_left_out` and `clearing_exclusive_restores_the_member_on_home` fail. The file was restored byte-for-byte (`diff` clean).

## 4. Home cap 1000 (D10a) — executed, with a scan fix

Cap tests (`tests/phase4_1_home_cap.rs`, application layer, accounts created with a placeholder hash so 1000 accounts are fast):
- `a_viewer_following_the_cap_sees_a_post_from_the_thousandth_follow`: 1000 follows, the oldest follow's post is read; 1000 statuses in total.
- `a_follow_beyond_the_cap_is_not_read`: 1001 follows; the oldest follow's post is not read; the newest is.
- `a_reblog_of_a_stranger_still_shows_with_a_full_follow_list`: a followed account's reblog of a stranger is on the first page at 1000 follows.
- `home_scan_at_the_cap_stays_within_the_bounded_scan_limits`: one page at 1000 follows: `items_scanned=2001`, `read_txns=5` (limits 5000 and 600).

Mutation checks (each restored byte-for-byte):
- `ACCOUNT_CACHE_CAP` 2000 → 1000: `a_reblog_of_a_stranger_still_shows_with_a_full_follow_list` fails (the cache fills with the 1000 follows, so stranger lookups fail closed).
- store `AUTHORS_CAP` 1001 → 500: `a_viewer_following_the_cap_sees_a_post_from_the_thousandth_follow`, `a_follow_beyond_the_cap_is_not_read` and the reblog test fail.

**Dense follow sets: a bounded-scan failure, found and fixed.** The ignored measurement `measure_dense_follow_sets_at_the_cap` (`PER_AUTHOR=n cargo test --test phase4_1_home_cap measure_dense -- --ignored --nocapture`) gives 1000 follows, each author posting `n` times, and then one fresh post from the *oldest* follow (the newest post on the account).

| merge implementation | posts per author | fresh post on first page | fresh post read by paging | items_scanned | read_txns |
|---|---|---|---|---|---|
| window + sort (Phase 4 code, cap 5 000) | 1 | yes | yes | 2001 | 5 |
| window + sort | 3 | yes | yes | 4001 | 5 |
| window + sort | 6 | **no** | **no** (statuses read 6000 of 6001) | 6000 | 5 |
| window + sort | 10 | **no** | **no** (10000 of 10001) | 6000 | 5 |
| lazy k-way merge (cap 5 000) | 1 | yes | yes | 2001 | 5 |
| lazy k-way merge (cap 5 000) | 3 | yes | yes | 2027 | 5 |
| lazy k-way merge (cap 5 000) | 6 | yes | yes (6001 of 6001) | 2034 | 5 |
| lazy k-way merge (cap 5 000) | 10 | yes | yes (10001 of 10001) | 2037 | 5 |
| lazy k-way merge, cap 4 000 (shipped) | 1 | yes | yes | 2001 | 5 |
| lazy k-way merge, cap 4 000 (shipped) | 6 | yes | yes | 2034 | 5 |
| lazy k-way merge, cap 4 000 (shipped) | 10 | yes | yes | 2037 | 5 |

Reading: with the window-and-sort merge, `AUTHORS_MERGE_CAP` (5000) is spent on the newest follows in follow order, so a fresh post from the oldest follow is dropped entirely at six posts per author. That is the bounded-scan failure at 1000 that the brief asked to be measured. The same mechanism at the old cap of 500 would start near ten posts per author (5000 ÷ 500; arithmetic, not measured). The lazy k-way merge (`birchtree.md` §5 describes this design) walks each author's cursor only as far as the page needs, so author order no longer decides what is read.

Shipped budget: `AUTHORS_MERGE_CAP` = 4000 (the first run used 5000). With the follow scan (1000 entries, `following()`), the total `items_scanned` for a home request at the cap is at most 5000 by construction, which matches the Phase 4 limit. Measured values above. The worst case (a page mostly filtered out) is bounded by construction, not measured under filtering.

## 5. Other checks

- `cargo test -- --list` reports 241 tests; the one ignored is the measurement. `grep -c "^warning"` over the gate log is 0.
- Phase 4 timeline suite (23) and list suite (5) pass on the lazy merge: sparse-filter bounded scan, cursors, `Link` headers, list ownership.
- The starting-state files (domain-block correction, `docs/logs/2026-10-17-domain-blocks-correction.md`, the `state/logs` entries) are unchanged by this work apart from the docs listed in the state log.
