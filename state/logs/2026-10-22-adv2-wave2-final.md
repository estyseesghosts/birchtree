# 2026-10-22 adv2 Wave 2 + final gate

Implemented parent-side (post-Wave1 commit, no new target dirs):

- A5 (F11): fetch stages via `staging_root()` (cache volume).
- B4 (N6): reconcile rebuilds accounting only on change; ledger restore only on repair.
- C4 (F10): `terminal_at_ms` stamped on complete/quarantine/max-retry; prunes age by it with `due_at` fallback; read-first prune scans. Updated 44 `complete`/`quarantine` callers mechanically.
- D2 stage 1 (F3): audio/video completes as not-cached (operator chose success); row keeps reason; clients use `remote_url`. Updated `fix_media_class` deliberately.
- E4 docs: `remote-media-cache-plan` + `Current-State` swept.
- New tests: `adv2_a_staging`, `adv2_b_reconcile`, `adv2_c_prune`, `adv2_d_stream` (8 tests) green.

Verification: check green; new tests green; full test 105/106 (1 flaky `c_media_http` gate, passes in isolation); release build success (4m42s); restart-recovery green. Evidence in `docs/logs/2026-10-22-adv2-verification.md`.

E3/F14 report (cannot be done from repo): enable branch protection on `master` requiring the `check, test, release build` and `dependency audit` jobs. In `ci.yml`, optionally pin the toolchain and run the full smoke set (`smoke-all.sh`). Treat workflow edits as small and review closely. Current `ci.yml` runs check/test/release + phase-1 smoke + audit on `ubuntu-latest` with floating `stable`; no required checks are enforced (branch unprotected per audit).

Definition of done: every finding F1-F11,N1-N6 has red-to-green (adv2_* + updated `fix_media_class`) or explicit defer (F3 stage2 proxy separate design, F12 plan only). F13 resolved, F12 plan written, F14 reported above. No workstream edited files outside ownership (Wave1); Wave2 parent-owned. History shows parent-only commits with explicit paths.
