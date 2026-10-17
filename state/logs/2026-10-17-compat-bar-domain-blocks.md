# 2026-10-17 — Compatibility bar, read-time ancestors, roles, domain-block correction

Follows `state/logs/2026-10-17-scope-expansion.md` and `state/logs/2026-10-17-scope-expansion-2.md`. Those two logs are unchanged. Where this entry differs from them, this entry supersedes them.

Phase reference: `birchtree.md` §1 (new compatibility bar), §6.3, §7 (Phase 3 item 6; Phase 6; Phase 7.3), §8 (item 11), §9, §10.

## Operator decisions applied

| Decision | Applied in |
|---|---|
| **Compatibility bar:** an unmodified Mastodon client connects to birchtree and has an identical experience to connecting to Mastodon v4.7.3. Every deliberate difference is in a deviation register. | `birchtree.md` §1 “The compatibility bar”; §8 item 11; `docs/compatibility-profile.md` (“Compatibility bar”, “Deviation register”); About; Roadmap |
| **Q3, read-time ancestors:** fetch ancestors and descendants on read, for logged-in viewers only. | `birchtree.md` §7 Phase 6; `docs/federation-contract.md` section C (rewritten), “Remote thread completion” test list, non-goals; `docs/endpoint-manifest.md` context row; profile (Phase 6 bullet, D5) |
| **Q4, roles:** implement admin and moderator roles. | `birchtree.md` §6.3 and §7 Phase 7.3 (decided; the permission split and audit trail stay proposed); profile (open decisions); manifest admin section header; Feature-Matrix |
| **Q8, domain blocks:** correct to upstream's shape, the way upstream does it. | Code change (below); `birchtree.md` §7 Phase 3 item 6; manifest rows and discrepancy 1 (resolved); profile; Feature-Matrix; Current-State |

## Code changes (executed)

Domain-block correction, to the pinned v4.7.3 shape:

- `src/api/mastodon/router.rs`: `/api/v1/domain_blocks` now has `GET` (index), `POST` (create) and `DELETE` (destroy). The `PUT` route and `DELETE /api/v1/domain_blocks/{domain}` are removed.
- `src/api/mastodon/endpoints/relationships.rs`: `domain_blocks_index` pages newest first with `max_id`/`since_id` row-id cursors (limit 100, maximum 200), and builds `Link` `next`/`prev` from ids. `domain_blocks_create` and `domain_blocks_delete` read `domain` from the form body or the query string. A missing or blank domain is a `422 Validation failed: Domain can't be blank`.
- `src/api/mastodon/pagination.rs`: `link_header_desc` (`next` via `max_id`, `prev` via `since_id`). The shared helper now takes the list of parameters to drop.
- `src/application/relationships.rs`: malformed domains return `422 Validation failed: Domain is invalid`. `list_domain_blocks` takes `max_id` and `since_id`.
- `src/application/ports.rs`: `domain_blocks(account, max_id, since_id, limit)`.
- `src/domain/relationship.rs`: `DomainBlock` gains `id: Id`, the row's numeric id (upstream's `AccountDomainBlock` id).
- `src/persistence/redb/schema.rs`: `DOMAIN_BLOCKS_BY_ID` table (`account_be || desc(id)_be`). Schema version 4 → 5. Table name registered.
- `src/persistence/redb/relationships.rs`: create assigns `next_id()` for a new block and keeps the id for a re-block. Create and unblock write both tables in one transaction. Listing is a bounded range scan on the id index.
- `src/persistence/redb/migrations.rs`: step v5 creates the index and backfills pre-v5 rows. A legacy row gets `created_at_ms << 16 | r`, where `r` ranks the account's rows that share a creation millisecond in domain order. This keeps ids increasing with creation time. Rows that already carry an id are kept, so the step is idempotent.
- Tests: `tests/phase3_relationships.rs` (the domain-block test rewritten for the upstream shape, including `405` for the old `PUT`; a new paging test); `tests/phase4_timelines.rs` (`PUT` → `POST`); `src/persistence/redb/migrations.rs` (new v4 → v5 backfill test).
- `scripts/smoke-phase3.sh`: `POST` to block, `DELETE ?domain=` to unblock.

Verification (executed; full output summary in `docs/logs/2026-10-17-domain-blocks-correction.md`):

- `cargo check --all-targets`: `Finished`, zero warnings.
- `cargo test`: **221 passed, 0 failed** across 18 test binaries (219 at Phase 4 exit, plus two new tests; the migration test is one of them).
- `scripts/smoke-all.sh`: `SMOKE_PHASE4_OK` and `smoke-all: green`.

Not verified here: parity against a live Mastodon v4.7.3 instance, and any unmodified client. The parity suite (`birchtree.md` §8 item 11) does not exist yet.

## Documentation changes

- `birchtree.md`: the compatibility bar (§1); Phase 3 item 6 (the correction); Phase 6 (read-time ancestors, decided); Phase 7.3 (roles decided; permission split and audit trail proposed); §8 item 11 (the parity suite); §9 (open decisions: the Q3, roles and Q8 items are now recorded as decided; the default search mode is open).
- `docs/compatibility-profile.md`: “Compatibility bar”; “Deviation register” (D1–D13); domain-block bullet; Phase 6 and Phase 7.3 bullets; open decisions.
- `docs/federation-contract.md`: section C rewritten as adopted (D5); test list; non-goals.
- `docs/endpoint-manifest.md`: domain-block rows (`GET`, `POST`, `DELETE` in the upstream shape); context row (ancestors, D5); admin section (roles); search deviations (D6); discrepancy 1 resolved.
- `docs/logs/2026-10-17-domain-blocks-correction.md` (new): executed evidence.
- `docs/wiki/`: About (the bar); Roadmap (the bar); Feature-Matrix (domain blocks, thread, admin, parity-suite rows); Current-State (schema v5, 221 tests, domain-block shape).

## Deviation register: conflicts with the bar that need an operator decision

The bar says "identical experience". The following decided items conflict with it. The register records each one.

1. **D1 `gifv` never produced** — accepted by the operator (2026-10-17). Clients with `gifv` autoplay and loop show these as ordinary videos.
2. **D5 read-time ancestors** — accepted by the operator (2026-10-17). Logged-in contexts can show ancestors Mastodon would not.
3. **D6 search default `embedded`** — open. Mastodon's default install (Elasticsearch disabled) returns no status hits, so the `embedded` default returns hits it would not. The bar suggests `off` as the default. This needs an explicit operator choice.
4. **D11 JSON request bodies (gap)** — Phase 3 does not accept them, and clients that send JSON fail. It is planned for Phase 7 at present. The bar argues for moving it earlier. Operator to confirm the phase.
5. **D9 timeline `206` (accepted, unreviewed)** — fan-out on read differs from Mastodon's asynchronous regeneration. It has not been reviewed against the bar.
6. **D10 timeline gaps (gap)** — the 500-follow home cap, exclusive lists not applied to home, and the tag-feed reblog question. These must close before the bar is claimed.
7. **D3, D4 formats (open)** — the video target format and the still-image target format. Upstream's video format is not verified yet.

## Open questions

1. D6: default search mode, `embedded` or `off`.
2. D11: move JSON request bodies earlier than Phase 7?
3. Admin permission split (proposed: moderator covers reports and account actions; admin covers every admin route). Confirm, or use one admin role.
4. Audit trail for admin writes (proposed on).
5. First-party `/api/v1/*` calls use OAuth tokens (proposed) or the browser session.
6. D3 and D4: video and still-image target formats.

## Next step

Phase 5 (streaming) per `AGENTS.md`. Before any phase exit, the unmodified-client parity suite starts from Phase 3 (`birchtree.md` §8 item 11). Resolve open questions 1–2 before the bar is claimed for Phase 4.
