# 2026-10-22 — core-ext rebase onto master (Stage 1 core extensions)

Branch `rebase/core-ext-onto-master` from `origin/master` (`bbed231`),
re-applying `origin/feature/core-extensions` (`ed21657`, base `2abe044`)
per `docs/aperture-phase1-ext.md` (schema v18 → v22, emoji admin API/CLI kept
verbatim, CORE-L/P/T verbatim, duplicate remote-emoji ingest dropped in favor
of master's vetted v20 `REMOTE_EMOJI`/`federation/media.rs` path).

## Delegation

Seven parallel subagents with disjoint file sets (schema, config,
emoji-keep, CORE-L, CORE-P, CORE-T, docs/tests). Shared/parent-only files
(`docs/*`, `ports.rs`, `redb/*`, `config.rs`/`main.rs`, `router.rs`,
`domain/job.rs`, `jobs/*`, `tests/common/mod.rs`) merged by parent from
snippet proposals only. Subagents used uniquely-named test files and made no
commits. Native Windows tools only (PowerShell + native cargo, separate
`CARGO_TARGET_DIR` per agent); no WSL.

## What changed (parent merge)

- Schema v22: kept master's `schema.rs` wholesale, appended
  `PLACES`/`PLACES_BY_NAME` (comment rewritten to v22),
  `SCHEMA_VERSION = 22`, `TABLE_NAMES += places/places_by_name`;
  `migrations.rs`: kept v17–v21, deleted core-ext `<17` no-op block,
  converted `<18` to `if current < 22` (opens PLACES/PLACES_BY_NAME/STATUSES);
  `redb/places.rs` verbatim (120 lines, hash `5f5ddf7` == core-ext blob) +
  `mod places;`.
- Minimal wiring for lib-green: `domain::{license,place}` mods + re-exports,
  `application::{license_policy,place_policy,alt_text_policy}` mods,
  `ports::PlaceStore` + `Store` supertraits.
- Config: `license_policy`/`place_policy`/`alt_text_policy` (`off` defaults,
  strict validation, `--*-policy=` flags) + schema/docs rows. No
  `aperture_mode` (stays on `feature/aperture-mode`).
- Emoji-keep: `admin_emoji.rs` verbatim (hash `d2ebfe5`), `EmojiStore::all_emojis`
  (ports + redb impl), router mounts after `reports/:id/resolve`, CLI
  `add-emoji`/`remove-emoji`/`rm-emoji`/`list-emojis` + `help`. Dropped:
  `remote_emoji.rs`, `RemoteEmoji::from_tag`, ingest path, per-tag enqueue,
  `tests/emoji_remote_ingest.rs`.
- CORE-L: ports fields, `MediaAttachment.license`, `Account.default_license`,
  enforced gate in create/update, scheduled persist (`off` replay default),
  endpoint parse, DTO `license`/`preferredLicense` (omitted when `off`),
  AP `Attachment.license` emit/parse (actor `preferredLicense` deferred).
- CORE-P: ports `place_id` fields + `PlaceStore`, `Status.place_id`, gates,
  `location` DTO (omitted when `off`), AP `location` emit + `link_remote_place`
  (same-name local wins). No geo-search (stub boundary kept).
- CORE-T: `alt_text_policy` enforcement in media upload/update + status
  create/edit over resulting set, `X-Birchtree-Alt-Text-Nag: 1`, scheduled
  persist, federation no-drop kept.
- Federation: `FedContext` + `build_config(_with_remote_cache)` gain the two
  policies; `note_for_status`/`question_for_status`/`note_inner`/`create`/
  `update`/`question_update`/`quote_request` + all 8 call sites;
  `wire::Attachment.license` + `wire::WirePlace`/`Note.location`;
  `note_input_for` keeps master's 3-arg arity + license map + location;
  `RemoteAttachment.license`, `RemotePlace`, ingest link.
- Tests: `tests/common` gains `with_config` + `assemble_with_cfg` (both
  regions kept); 15-suite touch-ups per plan (struct literals, trailing
  policy tuples, `note_input_for` 3-arg, `outgoing::create` +2 args,
  `build_config` +2 args in `a_signed_media_fetch`); probe files
  `z_schema_v22`/`z_config_policy`/`z_docs_touchup`; flipped config probe to
  post-merge expectations.
- Docs: manifest (5 hunks incl. new admin-emoji table), compat-profile
  1-line update, federation-contract G-D remote-emoji section rewritten to
  describe master's vetted path (not the dropped module), configuration.md
  section appended byte-exact (U+2192 preserved), TODOs zero-diff (D32c
  already landed).

## Verification (real output)

- `cargo check --all-targets`: EXIT=0. Only warnings: pre-existing
  `transcode.rs:742 unused pid`, pre-existing lib-test `remote_cache.rs:721`
  dead code, pre-existing rustfmt import-order drift (confirmed on untouched
  `actor.rs`; newer rustfmt version noise — left verbatim, no reformat).
- `cargo test` (full): all suites `ok`, 0 failed. Exact plan counts:
  `coreext_license` 17/17, `coreext_places` 13/13, `coreext_alttext` 8/8,
  `emoji_admin_api` 4/4, `emoji_cli_mgmt` 4/4, `gaps_federation_tail` 19/19
  (no ingest regression). Neighbors green: phase4_media 8/8,
  phase3_status* 13+4/13+4, phase4_scheduled 3/3, phase6_content 32/32,
  phase6_federation 17/17, round3_remote_timeline 5/5, fix4_p2_fedpush 14/14,
  phase7_web_odds 10/10, storage_* 3+7+6, config lib 17 + config_file 6,
  all fix4/gaps/phase7/round3/round4 suites ok (2 pre-existing ignores).
  One self-inflicted probe failure fixed mid-run (probe asserted
  load-rejection; `load_from_file` doesn't validate — now asserts via
  `validate()`); final rerun 2/2 + 3/3 + 2/2 probes green.
- `scripts/smoke-all.sh`: `cargo check` + `cargo test` phases fully green
  inside the script run; `smoke-phase1.sh` fails at server start with
  `secret keyring: no secret keyring configured ...; refusing to start in
  production mode`. PRE-EXISTING master breakage, not this branch:
  `smoke-phase1.sh` last touched in `2149d90` (pre-hardening),
  the keyring refusal landed in `01a0152` (Step 1 prod-hardening); script
  and refusal code are master's verbatim (scripts/ untouched here).
  Equivalent manual smoke with `--lab` on the merged binary: /health ok,
  /ready ready, /nope 404, create-account ok
  (id=117482196973780992), list-emojis exit 0, restart preserves identity.
- Parity-off: covered by the off-mode tests (all green):
  `off_ignores_license_params_and_omits_wire_keys`,
  `off_ignores_place_id_and_omits_location`, `off_changes_nothing` —
  wire omits `license`/`preferredLicense`/`location`, no nag header.

## Open questions

1. `media.rs` endpoint pre-parses non-numeric `license` to 422 even under
   `off` (numeric garbage ignored, non-numeric 422s) — verbatim core-ext
   split; confirm intended vs endpoint skipping pre-parse under `off`.
2. POST-create 422s unparsable `place_id` even under `off`, PUT ignores it
   under `off` — verbatim asymmetry; confirm intended.
3. `coreext_places.rs` had 2 unused imports from verbatim copy (removed here);
   `alt_text_policy.rs` has 1 pre-existing rustfmt collapse nit (kept verbatim).
4. `smoke-phase*.sh` need a `--lab`/keyring update for the post-hardening
   tree — hardening track owns that; this branch proves the binary end to end.
5. `feature/aperture-mode` (`655031a`, stacked on `ed21657`) rebases AFTER
   this lands, per plan §1.
