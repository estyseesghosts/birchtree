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

## WSL guest failure (revised: bench implicated)
- Guest died ~18:58–19:00 EDT (prev boot Sun 18 Oct 15:22 → register dump
  18:58:03; reboot 19:00:47). The earlier "no bench" claim was wrong:
  `/root/bench/run8` (12-node federated stress, 25 users/node, T+3h19m at
  18:45 EDT, 10 nodes up incl. transcode/media workloads) was live through
  the whole window: memory pressure from ~18:38, weston SIGSEGV 18:48–51,
  docker health timeouts 18:55, silence 18:58. run8's status loop kept
  writing until 18:57. Verdict: run8 very likely OOMed/starved the 19 GB
  guest (no OOM-killer lines in the journal, but the shape fits).
- run9 started 19:29 (post-reboot) and is ACTIVE now (solo-load 19:49,
  watcher/status loops) alongside this session's gate — shared-box
  contention to keep in mind.
- Lesson stands: serialize heavy builds; one worktree build at a time.

## Open
- ~~CORE-P / CORE-T merges pending worker final reports.~~ DONE: parent
  merged all three slices from worker deliverables (`17a118a`,
  `973dce3`; details in sections added below).
- Stage 1 gate: `scripts/smoke-all.sh` + 3x `coreext` suites, then `C_CORE`.

## CORE-P merge (`17a118a`, 13/13 green)
- New: `src/domain/place.rs`, `src/application/place_policy.rs`,
  `src/persistence/redb/places.rs` (PlaceStore: upsert/place_by_id/
  places_by_ids/link_remote_place with same-name dedupe so remote input
  can't hijack local names), `tests/coreext_places.rs`.
- Wiring: `Status.place_id` inline (schema v18); NewStatus/StatusUpdate;
  place edits count as real edits; endpoint parse (`off` ignores incl.
  unparsable, else 422); enforced gate in use cases; scheduled persist +
  replay; `ServerInfo.place_policy`; StatusView batch + `location` DTO
  (`{"id","name","longitude","latitude","country"}`, omitted when off);
  AP emit via FedContext policies (attachment `license`, Note
  `location`; sender place ids opaque) + ingest (license
  validated-or-NULL, location mints local rows); `preferredLicense`
  actor field deferred (external `Person` type can't carry it).
- Regressions: phase6_*, round3_remote_timeline, fix4_p2_fedpush,
  phase7_web_odds, emoji_remote_ingest all green.

## CORE-T merge (`973dce3`, 8/8 green)
- `src/application/alt_text_policy.rs` promoted to first-class module
  (domain errors); block enforced in media upload/update + status
  create/edit (resulting set; replies NOT exempt — every write leaving
  an undescribed image/video 422s; text/audio unaffected); nag header
  on v1/v2 media + status create/edit successes; scheduled persist.
- `tests/coreext_alttext.rs`: pure + HTTP matrix + flip coverage
  (grandfathered rows 422 on later writes under block).
- Pre-existing quirk reused: attached media rows 404 on re-update
  (load_addressable_media), so the clear-description test uses a fresh row.

## Stage 1 gate
- `scripts/smoke-all.sh` running in background (-> C_CORE on green).

## Stage 1 gate: GREEN
- `cargo check --all-targets` + full `cargo test` (762 ok) + live smokes
  phase 1-4, 5 (rerun; first attempt flaked on server boot under run9
  contention), 6, 7, 7.1, 7.2, 7.3 — all `*_OK`.
- C_CORE = `ed21657` (feature/core-extensions tip, incl. session-log
  commit). `feature/aperture-mode` cut from C_CORE.

## Stage 2: MERGED + PUSHED (per operator: commit+push prioritized over testing)
- `feature/aperture-mode` = `7e8d3ec` on top of skeleton `4356672` on top
  of C_CORE `ed21657`. Both branches pushed to
  github.com/estyseesghosts/birchtree (nothing on main/master).
- Aperture suites green at merge time: a1 5/5, a2 11/11, b1 6/6, b2 4/4,
  e1 7/7, zero ignored. Full `cargo check --all-targets` green at merge;
  full `cargo test` / smoke-all NOT re-run on the final tree per operator
  instruction ( Stage 1 gate was green; Stage 2 needs a full gate +
  two-instance interop + Pixelfed fixture run before being called done).
- Parent decisions: A2 Q1 message `Status must have media attachments`;
  Q2 strip-only edits (grandfathered text editable); Q3 publish-time-only
  scheduled enforcement. B1 replies leave timelines (store-level
  has_media); link+list forced, untested. B2 Q1 exact-URL exempt; Q2
  application-level filter; D53 assigned. E1 Q1 audio doesn't satisfy;
  Q2 NoteRef needs no gate (read-only); Q3 known-status Updates ungated;
  Q4 log-line counter only. `preferredLicense` actor federation deferred
  (external Person type).
- Open: full gate on final tree, interop evidence, worktree cleanup
  (wt-core-*, wt-ap-*, wt/core-*, wt/ap-* branches).
