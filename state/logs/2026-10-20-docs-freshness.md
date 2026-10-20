# 2026-10-20 — docs freshness pass (W1–W4, docs-only)

Four workers, disjoint file sets, all left uncommitted; parent reviewed diffs,
made 3 small consistency fixes (wiki announcement-authoring wording: admin API
only, no CLI path exists — `src/main.rs` has no announce command; `Home.md`
status line → 2026-10-20 / Phase 8 next), then committed.

- W1 plan+phases: `birchtree.md` §6.3/§7/§8/§9 status notes (fix round DONE,
  Phase 8 next, Phase 6 partial), `docs/README.md` rows (`phase-6`, `TODOs`,
  `fix-plan.md` → Shipped), dated `Status 2026-10-20` addenda on
  phase-0..6 exit criteria (no boxes changed).
- W2 contracts: manifest planned→implemented on privacy_policy/terms rows,
  scope-registry item 5 resolved, bullet/not-yet-rowed lists pruned;
  compat header fix + scheduled-status vs scheduled-announcement correction;
  visibility mute-denial fix (F2); configuration CLI-flag list + smtp fixes;
  persistence `SCHEMA_VERSION = 16`.
- W3 audits+TODOs: `fix-plan.md` COMPLETE block, `federation_repair.md`
  addendum (Boost fixed, remote_url carried by TODOs), remote-cache stub
  confirmed, `testing.md` gate + suites table, source-audit parenthetical.
  TODOs unchanged: full sweep found nothing new meeting the cite bar.
- W4 wiki: jargon-free rewrite of About/Current-State/Roadmap/Feature-Matrix,
  accurate to HEAD (re-checked vs W2 changes at parent gate).

Coverage question (aperture/clips/pages/antennas): zero hits in `src/`,
router, manifest, and TODOs for aperture/clip/antenna; "pages" only as the
`indexable_statuses_page` backfill port and the pagination sense. Whether
`docs/aperture.md` (other session's untracked file, unread per instructions)
defines surfaces needing TODO rows is undetermined — routed to aperture.md's
owner, not filed here.

Verification: docs-only, no cargo per handoff; every worker claim cites
`src/` at HEAD (`7d6e386`). `git status` clean except `?? docs/aperture.md`,
`?? docs/misstodon.md` (other session's, untouched).
