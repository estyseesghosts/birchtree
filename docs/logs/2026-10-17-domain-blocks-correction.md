# 2026-10-17 — Domain-block correction: verification evidence

Phase reference: `birchtree.md` §7 Phase 3 item 6 (correction). Work log: `state/logs/2026-10-17-compat-bar-domain-blocks.md`.

Change under test: the domain-block endpoints move to the shape pinned at Mastodon v4.7.3 (`config/routes/api.rb`: `resource :domain_blocks, only: [:show, :create, :destroy]`; `app/controllers/api/v1/domain_blocks_controller.rb`). Schema v4 → v5 (`DOMAIN_BLOCKS_BY_ID`, backfill).

## Commands and results (executed)

`cargo check --all-targets` (after the final edit):

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.19s
```

Zero warnings after the last change (the unused-import warning was removed).

`cargo test` (full run, final code):

```
test result: ok. 72 passed; 0 failed   (src/lib.rs unit tests)
test result: ok. 0 passed; 0 failed    (src/main.rs)
test result: ok. 3 passed; 0 failed    (tests/crash_recovery.rs)
test result: ok. 18 passed; 0 failed   (tests/persistence_conformance.rs)
test result: ok. 13 passed; 0 failed   (tests/phase2_api.rs)
test result: ok. 13 passed; 0 failed   (tests/phase2_oauth.rs)
test result: ok. 8 passed; 0 failed    (tests/phase3_interactions.rs)
test result: ok. 14 passed; 0 failed   (tests/phase3_relationships.rs)
test result: ok. 13 passed; 0 failed   (tests/phase3_status.rs)
test result: ok. 15 passed; 0 failed   (tests/phase4_filters.rs)
test result: ok. 5 passed; 0 failed    (tests/phase4_lists.rs)
test result: ok. 8 passed; 0 failed    (tests/phase4_media.rs)
test result: ok. 4 passed; 0 failed    (tests/phase4_notifications.rs)
test result: ok. 3 passed; 0 failed    (tests/phase4_scheduled.rs)
test result: ok. 3 passed; 0 failed    (tests/phase4_search_emoji.rs)
test result: ok. 23 passed; 0 failed   (tests/phase4_timelines.rs)
test result: ok. 3 passed; 0 failed    (tests/runtime.rs)
test result: ok. 3 passed; 0 failed    (tests/secret_handling.rs)
TOTAL passed=221 failed=0
```

The total is 221: the 219 recorded at Phase 4 exit, plus two new tests:

- `tests/phase3_relationships.rs::domain_blocks_crud_and_relationship_flag` (rewritten to the upstream shape, including the old `PUT` now returning `405`).
- `tests/phase3_relationships.rs::domain_blocks_page_by_row_id_like_upstream` (new: newest-first order, `max_id` and `since_id` exclusive cursors, `Link` `next`/`prev`, no `next` on a short page).
- `src/persistence/redb/migrations.rs::upgrades_v4_database_backfills_domain_block_ids` (new: legacy v4 rows get deterministic ids, ranked within a millisecond in domain order; the id index is newest first per account).

`scripts/smoke-all.sh` (final code; runs `smoke-phase1.sh`, `smoke-phase2.sh`, `smoke-phase3.sh`, `smoke-phase4.sh`). The captured output was the tail of the run:

```
SMOKE_PHASE4_OK
smoke-all: green
```

`smoke-phase3.sh` now sends `POST /api/v1/domain_blocks` and `DELETE /api/v1/domain_blocks?domain=…`. Its domain-block lines are inside the run that reported `smoke-all: green`. The head of the output was not captured separately.

## Behavior checked against upstream

- Newest first; `max_id`/`since_id` are exclusive numeric row ids; limit 100, maximum 200.
- Blank domain → `422 Validation failed: Domain can't be blank`; malformed → `422 Validation failed: Domain is invalid` (upstream's `RecordInvalid` and `DomainValidator`).
- `DELETE` is idempotent and takes `domain` from the query string or the form body.
- Scopes are unchanged: read `follow | read | read:blocks`; write `follow | write | write:blocks`; `requires_user` → `422`.

## Not covered here

- Upstream's `AfterAccountDomainBlockWorker` (removing remote followers from a blocked domain) has no effect until federation (Phase 6).
- Parity against a live Mastodon v4.7.3 instance: not run. The unmodified-client parity suite (`birchtree.md` §8, item 11) is not built yet.
