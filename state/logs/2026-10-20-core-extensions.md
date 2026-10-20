# 2026-10-20 — Stage 1 core extensions: skeleton + CORE-L merged

Base pinned: `ca901c9` (master). Branch `feature/core-extensions`.
Untracked `docs/aperture.md` / `docs/misstodon.md` left on disk, never
modified (accidentally staged once in `d895bf5`, removed from the branch
in `e63b39a` via `rm --cached`; disk copies intact).

## Parent skeleton (`46261ab` + `923a7bd`)
- `Config::{license_policy, place_policy, alt_text_policy}` (three-state,
  defaults `off`/`off`/`off`) + validation + `--license-policy=` /
  `--place-policy=` / `--alt-text-policy=` CLI flags + `docs/config-schema.json`
  + `docs/configuration.md` core-ext section.
- Schema v17 (licenses inline in `media`/`accounts` JSON blobs, open-only)
  + v18 (`places`, `places_by_name` tables; `Status.place_id` inline).
  Pinning test `tests/config_file.rs` covers new keys generically (no edit).
- `cargo check --all-targets` green at commit.

## Slice workers (disjoint worktrees, new-files-only + snippets)
- wt/core-license → DONE, merged (below). Interim: no report until final.
- wt/core-places → 4 new files in place; test run pending (cargo lock).
- wt/core-alttext → pure helpers + matrix in place; nag = response header
  `X-Birchtree-Alt-Text-Nag: 1`; verification pending.

## CORE-L merge (`d895bf5`, 17/17 green)
- New: `src/domain/license.rs` (Pixelfed ids 1,5,6,11-16, `parse_license`),
  `src/application/license_policy.rs` (effective/inheritance, enforced gate,
  wire mapping), `tests/coreext_license.rs` (pure + HTTP matrix).
- Wiring: `MediaAttachment.license`, `Account.default_license` (serde
  default; schema v17); `NewMedia{,_Upload}.license`, `MediaPatch.license`,
  `AccountProfileUpdate.default_license`; upload/update media params
  (off skips parse entirely — Q1 decided: garbage ignored too);
  `update_credentials[default_license]`; enforced gate in
  `create_status`/`update_status` (+`license_policy: &str` params);
  scheduled rows persist the policy (`license_policy` param key, old rows
  replay as `off`); `ServerInfo.license_policy`; DTO `license` /
  `preferredLicense` render only when enabled.
- Manifest rows for media/statuses/update_credentials.
- Regressions green: phase4_media, config_file, phase7_gaps_round2_a,
  round3_heic_tiff, phase7_1_transcode, phase3_status(_limit),
  phase4_scheduled.
- Pre-existing quirk noted (not fixed): PUT statuses only recognizes exact
  `media_ids`, not `media_ids[]` (`forms::get` vs `forms::all`).
- AP wire emit/parse (`license` on attachments, `preferredLicense` on
  actor) NOT yet landed — remote rows store NULL; follows in Stage 1
  before gate (or documented deferral).

## WSL guest failure (separate investigation)
- Guest died ~18:58–19:00 EDT (prev boot Sun 18 Oct 15:22 → register dump
  18:58:03; reboot 19:00:47). No bench process exists
  (`scripts/bench-local.py` was never created; nothing in history/ps).
- Cause: sustained memory pressure from ~18:38 (journald/resolved cache
  flushes), weston SIGSEGV 18:48–51, docker health timeouts 18:55. No
  OOM-killer lines; likely host-level WSL restart under pressure.
- Lesson: three parallel full worktree builds (~2.7 GB RSS each, load ~25)
  recreate that shape — serialize heavy builds in Stage 2.

## Open
- CORE-P / CORE-T merges pending worker final reports.
- Stage 1 gate: `scripts/smoke-all.sh` + 3x `coreext` suites, then `C_CORE`.
