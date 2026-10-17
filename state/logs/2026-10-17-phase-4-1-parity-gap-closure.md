# 2026-10-17 — Phase 4.1: parity gap closure (D11, D10b, D10a)

Follows `state/logs/2026-10-17-media-and-phase-4-1-decisions.md` (the handoff) and `state/logs/2026-10-17-parity-decisions.md`, which stay unchanged. Evidence: `docs/logs/2026-10-17-phase-4-1-verification.md`. Nothing committed.

## Read first (as briefed)

`AGENTS.md`; `birchtree.md` §1 (bar), §5 (timeline strategy), §7 Phase 4.1, §8 item 11; `docs/compatibility-profile.md` (D9, D10a–c, D11); `docs/endpoint-manifest.md` (Phase 3 JSON note, Phase 4 timeline deviations, write rows); both 2026-10-17 decision logs.

Starting state: uncommitted 2026-10-17 domain-block changes and docs, left in place. Last measured numbers held (221 tests, check clean, smoke green).

## Outcome

| Item | Result |
|---|---|
| D11 JSON bodies on REST write endpoints | implemented; 43-case JSON ≡ form table; malformed JSON answered as upstream's source says |
| D10b exclusive lists hidden from home | implemented in the viewer-facts policy; 5 HTTP tests |
| D10a home cap 1000 + account cache | implemented (cache 2000, store author cap 1001); boundary, beyond-cap, stranger-reblog and bounded-scan tests |
| Bounded-scan limits at 1000 | held, but only after fixing a real merge defect (below) |
| Gate | `smoke-all: green`; check zero warnings; 240 passed, 0 failed, 1 ignored (measurement); 241 total |
| §8 item 11 parity harness | **not built, not run**. Parity evidence is internal (JSON ≡ form) plus source-derived for upstream. See Open items A |

## What changed

Code:
- `src/api/json_body.rs` (new): flattening (`parse`, `body_pairs`), the malformed-JSON `guard` middleware, the upstream error answer, and `Accept` negotiation. Four unit tests.
- `src/api/forms.rs`: `LenientForm` is JSON-aware; new `FormBody` extractor for the former `RawForm` sites (keeps axum's 415 for other content types).
- `src/api/mastodon/router.rs`: guard layer, placed inside the rate limiter (Rack::Attack runs first upstream).
- Handlers: `accounts.rs` (register, update_credentials → `FormBody`), `apps.rs` (→ `FormBody`), `oauth/token.rs` (token, revoke → `FormBody`), `markers.rs` (body read through `body_pairs`), `lists.rs` (create reads `replies_policy` and `exclusive`).
- `src/application/timelines.rs`: `HOME_AUTHOR_CAP` 1000; `ACCOUNT_CACHE_CAP` 2000; `ViewerScope` gains the exclusive set, `seed_exclusive` and `on_exclusive_list`; `HomeFilter` applies the rule after the viewer's own-status return. List timelines do not call it.
- `src/application/ports.rs`, `src/persistence/redb/lists.rs`: `ListStore::exclusive_list_members(owner, candidates)` (one read transaction; point lookups in `LIST_MEMBERS`, cost = owner's exclusive lists × candidates); `create_list` takes the policy and the exclusive flag.
- `src/application/lists.rs`: `create` takes `replies_policy` and `exclusive`; shared `parse_policy`.
- `src/persistence/redb/timelines.rs`: **lazy k-way merge** replaces the per-author window and sort in `authors_timeline`; `AUTHORS_CAP` 500 → 1001; `AUTHORS_MERGE_CAP` 5000 → 4000; `AUTHOR_WINDOW` removed.

Tests (new): `tests/phase4_1_json_parity.rs` (6), `tests/phase4_1_exclusive_home.rs` (5), `tests/phase4_1_home_cap.rs` (4 + 1 ignored measurement).

Docs: `docs/endpoint-manifest.md` (Phase 3 JSON note resolved; new "Phase 4.1: JSON bodies and exclusive lists" section; home, list and account-cap notes), `docs/compatibility-profile.md` (D10b, D11 implemented; D10a updated; Phase 4.1 bullets), `docs/persistence-conformance.md` (merge row), `docs/wiki/Roadmap.md` (4.1 implemented), `docs/wiki/Feature-Matrix.md`, `docs/wiki/Current-State.md`.

Not changed, as required: `docs/phase-4-exit-criteria.md`, earlier `docs/logs/`, `AGENTS.md`. `birchtree.md` is also unchanged (its §7 Phase 4.1 text is the requirement, not a status line).

## Decisions I took (reversible; please confirm or redirect)

1. **Account cache 2000, not 1000.** The brief said "at least 1000". At exactly 1000 the cache fills with the 1000 home follows, and any later lookup (a reblog target from a stranger, a reply parent) fails closed, so a followed account's reblog of a stranger disappears. `a_reblog_of_a_stranger_still_shows_with_a_full_follow_list` fails at 1000 (mutation-checked).
2. **Store `AUTHORS_CAP` 1001** (not in the brief). The store truncated the author list at 500 with `take(AUTHORS_CAP)`, which would silently drop the 1000th follow even with the application cap raised.
3. **Lazy k-way merge with `AUTHORS_MERGE_CAP` 4000. This is the change to review most closely.** The brief's fallback for a bounded-scan failure at 1000 was "keep 500 and report". The failure was real and I measured it, but I did not revert: reverting reopens D10a, which is decided. Instead I replaced the store's merge. The old code took a window from every author and sorted, so the 5000-entry budget went to the newest follows; at 1000 follows with six posts each, a fresh post from the oldest follow vanished from home (measured, table in the verification log). The new merge walks each author's cursor only as far as the page needs (the design `birchtree.md` §5 describes). The budget drops to 4000 because the follow scan adds 1000 `items_scanned` on top. The same function serves the list timeline, so list reads change too; the phase-4 timeline and list suites pass. If you prefer the fallback (500), it is a one-constant change plus a revert of this merge.
4. **List create honours `replies_policy` and `exclusive`.** Upstream's `ListsController#create` takes both through `list_params`; birchtree's create ignored both, so a client creating an exclusive list got a non-exclusive one and D10b could never apply to it. An invalid `replies_policy` answers 422 with the update path's message; upstream's answer for an invalid enum is not verified here.
5. **Arrays of objects flatten to indexed keys** (`keywords_attributes[0][keyword]`), matching the form encoding the filter handlers already read. Scalars repeat `a[]`. A top-level non-object body is wrapped under `_json`, as Rails does.
6. **The malformed-JSON guard runs on every route**, before handlers and before authentication, inside the rate limiter. Rails parses params before the action, so a malformed body fails even on a body-less route. JSON bodies above 1 MiB answer 413.
7. **`POST /oauth/authorize` stays form-only.** It is the browser consent form. Upstream guards it with a CSRF token, so a JSON client cannot submit it; a JSON body answers 415 as before.
8. **Non-JSON, non-form bodies are unchanged.** `LenientForm` routes still parse them as forms; `FormBody` routes still answer 415.
9. **Upstream's malformed-JSON answer is derived from Rails 8.1.3.1 and the v4.7.3 tree** (no `public/400.html` exists, so `ShowExceptions` passes the 404 cascade through as an empty 400). "Unclear" would have meant stopping; it was not unclear.

## Verification

- `bash scripts/smoke-all.sh`: `smoke-all: green` (phase 1–4 smokes each print their `SMOKE_*_OK`).
- `cargo check --all-targets`: no warnings.
- `cargo test`: 240 passed, 0 failed, 1 ignored (the dense measurement, run with `--ignored`).
- Mutation checks, each restored byte-for-byte: account cache 1000 → the stranger-reblog test fails; store author cap 500 → three boundary tests fail; exclusive rule disabled → three exclusive tests fail.
- Dense measurement (verification log §4): the failing case before the merge change, the passing case after, with `items_scanned` and `read_txns` for each.

## Open items and escalations

A. **Parity is not run against a live v4.7.3.** `ruby`, `bundle`, `psql` and `redis-server` are not installed here, so the pinned instance cannot run. The §8 item 11 harness does not exist. The JSON answers are source-derived (Rails 8.1.3.1, Doorkeeper 5.9.9, the v4.7.3 tree and FeedManager at `8ff11c2b`). The phase's exit asks the evidence to state which surfaces it does not cover: the live comparison covers none of Phase 0–4.1 yet.
B. **The merge change needs review** (decision 3). Also: the register's D10a effect ("only accounts following more than 1000 miss older follows") was inaccurate on the Phase 4 merge for dense follow sets; it holds now, by measurement, for the cases measured. Sparse-filter worst case under the new budget is bounded by construction, not measured.
C. **Residual differences** (none is in the register): a malformed JSON body to an unknown `/api/` path answers 400 where upstream answers 404 (the guard runs before routing); JSON above 1 MiB answers 413; the `Vary: Authorization` header is not replicated on the malformed-JSON answer; invalid `replies_policy` on create is 422, unverified upstream.
D. **Pre-existing, outside 4.1, not fixed:** `PATCH /api/v1/accounts/update_credentials` takes form bodies only, so a multipart avatar or header upload gets 415. Upstream accepts multipart there. It belongs in a later account-profile slice.
E. **AGENTS.md** (not edited, per the brief): "Current phase" still names Phase 5 without noting 4.1, and the Phase 4 paragraph does not mention the 4.1 closure. Once you accept this change, the Phase 5 line is correct; the Phase 4 paragraph should say 4.1 is implemented. This needs your edit.
F. **`docs/phase-4-exit-criteria.md`** (off-limits) still lists exclusive lists, JSON and the 500 cap as open items. It is stale; it records the Phase 4 exit as it was.
G. **Commit:** the working tree holds the 2026-10-17 domain-block work, the docs, and this change, all uncommitted. Asked the operator whether to commit before continuing (see the final message).
