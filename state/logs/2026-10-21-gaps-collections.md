# 2026-10-21 — gaps batch G-C: collections + missing REST surface

Subagent G-C, disjoint owned files only; shared files untouched (proposed as
snippets for the parent merge). Changes left uncommitted; no
commit/stash/reset/checkout run.

## Owned files (created, all new — Step 0 stubs were declarations only)

- `src/application/collections.rs` — `CollectionStore` port trait (defined
  here; parent moves it to `ports.rs`), `CollectionRecord` / `CollectionChange`,
  use cases (CRUD, items add/remove/list, revoke, `in_collections`,
  `members_visible`), `fanout_payload` + `handle_fanout` (`CollectionFanout`
  body). Mutations commit record + membership rows + `items_count` + bounded
  fan-out job (≤500 ids, no blobs) in one txn; notifications fan out after
  commit via `fan_out_from_payload` (D26 gate + policy pipeline + dedup
  idempotency apply).
- `src/persistence/redb/collections.rs` — `impl CollectionStore for RedbStore`
  over `COLLECTIONS` / `COLLECTION_ITEMS` (schema v18, `collection_item_key`);
  per-account cap 50, per-request batch cap 500, per-collection cap 1000;
  `reblogged_by_distributable` (D52: public/unlisted wrappers only).
- `src/api/mastodon/endpoints/collections.rs` — all 22 G-C handlers as plain
  fns over `&dyn Store` / `&dyn CollectionStore` + `ROUTES` table + collection
  DTO. Plain fns (not axum handlers) because `Context.store` is
  `Arc<dyn Store>` and the port joins `Store` only at the parent merge; the
  report carries the 3-line-per-route axum adapter snippet the parent pastes.
- `tests/gaps_collections.rs` — 12 tests, all passing.

## Verified (real output)

- `cargo test --test gaps_collections` → **12 passed, 0 failed** (CRUD exact
  keys, items add/remove/paging, revoke + bounded job rows, hide_collections
  members-hidden/counts-kept, in_collections + account collections/lists,
  preview counts, peers search + gate, v2 family smoke, distributable scope,
  fanout notify + idempotent retry, routes table).
- `cargo check --all-targets` → green; zero diagnostics from G-C files
  (remaining warnings pre-exist in other agents' files).
- `cargo check --lib` → green.

## Design decisions (birchtree-shaped, documented in code)

- `public` defaults false; revoked is terminal for edits/adds (delete allowed).
- Show: owner sees all; others see public non-revoked only (404 otherwise).
- Items read: `hide_collections` → `200 []` for non-owners, `items_count`
  stays; every candidate passes the `show_status` viewer gate (no leaks).
- v2 group key `<type>-<anchor>` (status else actor else 0); groups derive
  from the newest 2000 rows; dismiss 404s on empty/unknown keys.
- `identity_proofs`, `email_subscriptions`, `GET/PATCH /profile` stay out
  (G-A owns profile; proofs/subs need crypto/mail surfaces that don't exist).

## Open for parent

- Mount the router snippet; move `CollectionStore` (+ record types) to
  `ports.rs` and add to the `Store` supertraits; replace the
  `CollectionFanout` no-op arm with the `handle_fanout` call; add the 22
  manifest rows + D30/D52-close + D51-narrow + TODOs deltas. All snippets are
  verbatim in the G-C final report.
- `familiar_followers` + `application/web.rs` featured reads still note "no
  `hide_collections` concept" — parent may extend the flag there (one-line
  checks against the same account row).
