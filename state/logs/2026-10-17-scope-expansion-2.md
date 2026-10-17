# 2026-10-17 — Scope expansion, follow-up: operator answers applied

Follow-up to `state/logs/2026-10-17-scope-expansion.md`, which is unchanged. Where this entry differs from that one, this entry supersedes it. Documentation only: no changes to `src/`, `tests/`, `scripts/`, `Cargo.toml`, `AGENTS.md`, phase-exit files, `docs/logs/`, or Phase 4 logs.

## Operator decisions applied

| # | Decision | Where it landed |
|---|---|---|
| 1 | Phase placement (Phase 6; Phase 7; 7.1–7.3; Phase 8) accepted as proposed | unchanged |
| 2 | keystodon is both a **first-party client** (birchtree serves its bundle; `/api/web/*` with browser session + CSRF) and a **third-party client** (OAuth against any host). `/api/web/*` is therefore **in profile**, not conditional | `birchtree.md` §1, §7 Phase 7, §8, §9, §10; `docs/compatibility-profile.md`; `docs/endpoint-manifest.md` (Web section, discrepancy 4); `docs/wiki/*` |
| 3 | Video codec is **VP9** (WebM container). Images, video **and especially GIF** are transcoded | `birchtree.md` §5 and §7 Phase 7.1; profile; manifest media deviation |
| 4 | **`gifv` is never produced.** Recorded as one of birchtree's deviations from upstream | `birchtree.md` §5, §7 Phase 7.1; manifest; profile; Feature-Matrix |
| 5 | Search engine: **embedded library**, and **optional**, as Mastodon and Pleroma allow it. See the search section below | `birchtree.md` §7 Phase 7.2, §3, §10; profile; manifest search deviations; Roadmap; Feature-Matrix |
| 6 | Crawl does **not** apply the same-host rule (matches upstream) | `birchtree.md` §7 Phase 6; `docs/federation-contract.md` (sections B, E, F and item 3) |
| 7 | `GET /api/v1/accounts` is **in profile**; phase chosen: **Phase 7** | manifest (new row); `birchtree.md` §7 Phase 7; profile; Roadmap; Feature-Matrix; Current-State; discrepancy 3 resolved |

### Search, from the pinned Mastodon and the Pleroma reference

- **Mastodon** (`v4.7.3`): `.env.production.sample` sets Elasticsearch as optional (`ES_ENABLED`). `SearchService#status_searchable?` requires `Chewy.enabled?`, so status search is off without it. Account search is on the database path.
- **Pleroma** (`~/fedifedifedi/pleroma`, commit `687fa6e1b`, `develop`): a pluggable `Pleroma.Search` with a `SearchBackend` behaviour (`search`, `add_to_index`, `remove_from_index`, `create_index`, `drop_index`, `healthcheck_endpoints`). The default backend is `DatabaseSearch`. Optional backends are Meilisearch, ParadeDB and Qdrant. Indexing only covers public and unlisted `Create`/`Note` activities (`indexable?`), and runs as Oban jobs (`SearchIndexingWorker`). `Search.Backfill` is bounded and resumable through a cursor.
- **Resulting birchtree design** (Phase 7.2): an embedded full-text library behind a search-backend port. The library is open at scoping, with candidates such as Tantivy to evaluate. Three modes are selectable by configuration: `embedded` (default), `database` (today's bounded scan, which is Pleroma's default behavior), and `off` (empty status results, matching Mastodon with Elasticsearch disabled). Indexing follows Pleroma's rule, tightened to public and unlisted only. Private and direct statuses are never indexed. Indexing and backfill are post-commit, resumable jobs. Accounts stay on the database path. Hashtag engine use is checked at scoping.

## Proposed defaults in the docs (not yet confirmed)

These are written as defaults so the plan is complete. Each is listed as open in `docs/compatibility-profile.md`.

1. **Animated GIF is typed `video`** (WebM/VP9). Without `gifv`, clients will not apply autoplay and loop treatment. This is a consequence of the decision, not a separate choice. Non-animated GIF counts as a still image.
2. **First-party `/api/v1/*` calls use OAuth tokens** issued to a first-party application. The browser session covers login, consent and `/api/web/*` only. This keeps one auth path for the API. The alternative is to let the session authenticate `/api/v1/*` too. Upstream does that for reads, and nulls the session for unverified writes.
3. **Admin roles: `admin` and `moderator`, granted through the CLI.** The split between them is not decided.
4. **Admin audit trail: on**, one record per admin write.
5. **Still-image target format:** open (candidates WebP, AVIF).

## Open questions, restated plainly

- **Q3 — read-time ancestors.** When someone opens a reply, should birchtree also fetch the post it replies to, if it doesn't have it? Mastodon does not. It fetches the parent once, when the reply arrives, and fetches replies below a post only when a logged-in user opens it. **Default (written):** match Mastodon, no. Say yes to add it as birchtree's own extension, which would need a recorded deviation.
- **Q4 — admin roles.** Admin actions need a role on top of the OAuth scope. Birchtree has no roles at all. **Proposed:** two roles, `admin` and `moderator`, set with a CLI command. Admin-only for domain, IP, email blocks and tags. Moderator for reports and account actions. Every admin write is logged. Confirm the split, or choose a single admin role for simplicity.
- **Q8 — domain-block shape (Phase 3, code).** Our domain-block endpoints do not match upstream v4.7.3. Upstream: `POST /api/v1/domain_blocks` with a `domain` parameter to block, `DELETE /api/v1/domain_blocks` with a `domain` parameter to unblock, and numeric `max_id`/`since_id` paging. Ours: `PUT /api/v1/domain_blocks` to block, `DELETE /api/v1/domain_blocks/{domain}` to unblock, and domain-string cursors. Clients built for upstream would fail against these routes. **Recommendation:** a small Phase 3 correction before Phase 5. This is a code change, so it has not been made. It needs your approval.

## Other changes in this follow-up

- `docs/endpoint-manifest.md`: added `GET /api/v1/accounts` (Phase 7, In, `read | read:accounts`, `id[]` up to 40). Corrected the domain-block `GET` note (upstream cursors are numeric, not domain strings). Reworded the `/api/web/*` intro. Reframed the media deviations (GIF and video → WebM/VP9, `gifv` never produced). Expanded the search-deviation note (`database`, `embedded`, `off`). Resolved discrepancies 3 and 4, and sharpened 1 with the cursor finding and the proposed fix.
- `docs/federation-contract.md`: crawl same-host rule removed (matches upstream). Added a residual-risk note on cross-host fan-out, bounded by the caps. Test case updated.
- `docs/compatibility-profile.md`: rewritten in full. Phase 7 now includes the first-party client and account multi-fetch. Phase 7.1 and 7.2 descriptions updated. Frontend exclusion reworded. Open decisions updated (keystodon mode resolved, search library, transcoding, admin roles, first-party auth).
- `birchtree.md`: §1 frontend bullet (two modes); §3 tree (`web_client.rs`, comments on `transcode.rs` and `search/`); §5 transcoding paragraph; §6.3 admin-role default; §7 Phase 6 crawl bullet; §7 Phase 7 (accounts bullet, first-party client bullet); §7 Phase 7.1 (decided and open lists); §7 Phase 7.2 (rewritten for the embedded, optional design); §7 Phase 7.3 prerequisites (role default, audit default); §8 item 9; §9 non-goal and open decisions; §10 Pleroma reference and domain-blocks and accounts controllers.
- `docs/wiki/`: `About.md` (two modes), `Roadmap.md` (rewritten; Phase 7 focus, 7.1, 7.2, keystodon paragraph, non-goals), `Feature-Matrix.md` (rewritten; accounts multi-fetch row, first-party client row, third-party OAuth row, transcoding row, search row), `Current-State.md` (Phase 7 list).

## Full list of changes beyond the handoff (both sessions)

Consolidated so nothing is missing from the record. Items 1–8 were in the first session log. Items 9–12 are from this follow-up.

1. Added `docs/README.md` index row for `phase-4-exit-criteria.md` (coherence).
2. Corrected stale status lines in `docs/wiki/Home.md` and `docs/wiki/About.md` (Phases 0–4; Phase 4 gate).
3. Changed three discovery rows (WebFinger, NodeInfo, nodeinfo 2.0) from `Plan` to `In`. No `Plan` rows remain.
4. Added rows the handoff omitted: `GET /api/v1/trends/links`, `GET`/`DELETE /api/v2/suggestions`.
5. Notes-only changes to existing rows: context (Phase 6 header), `GET /api/v2/search` (interim), three domain-block rows (shape discrepancy).
6. Relabeled the manifest's bullet section ("Bullet-only entries") and pruned the bullets that became rows. `GET /api/v1/custom_emojis` remains a bullet and is listed as a gap, not rowed.
7. Added the discrepancy section to the manifest. The first session log lists the verified source differences.
8. Did not touch `docs/source-audit-v4.7.3.md` or `docs/domain-contracts.md`. Both are outside this handoff's file scope. The first log lists them as follow-ups.
9. Added `GET /api/v1/accounts` (Phase 7) to the manifest; resolved discrepancy 3.
10. Recorded Q8 as a proposed Phase 3 code correction (not applied).
11. Corrected my own earlier manifest text on domain-block cursors (upstream uses numeric ids, not domain strings).
12. Cited the Pleroma commit used for the search design (`687fa6e1b`, `develop`) in `birchtree.md` §10 and this log.

## Verification (executed)

- Manifest route rows: **217** (117 at HEAD + 99 from the first session + 1 accounts row). Rows with `Plan` profile: **0**.
- Stale-term scan across `birchtree.md`, `docs/`, `docs/wiki/`: remaining `keystodon`, `gifv`, `same-host` and `open decision` hits are either the new text or intentional references. Nothing still says the crawl applies the same-host rule, that keystodon's mode is open, or that `gifv` is produced.
- Not run: `cargo check`, `cargo test`, `scripts/smoke-all.sh`. Documentation only.

## Next step

Operator confirmation of: Q3 (default: no read-time ancestors), Q4 (roles), Q8 (approve the Phase 3 domain-block correction), and the five proposed defaults above. Then Phase 5, per `AGENTS.md`.
