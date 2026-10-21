# 2026-10-21 — avatar/header upload on update_credentials (handoff completion)

## What changed

Completed `HANDOFF_avatar-upload.md` on baremetal Windows (PowerShell only).
The prior implementer's code was already in the worktree uncommitted; this
session verified it, fixed the remaining red, and updated contracts. No
commits (per subagent rules — parent commits).

- `src/api/mastodon/endpoints/accounts.rs` — `update_credentials` branches
  on content type: multipart text parts flatten into the same pairs the
  urlencoded/JSON path parses (order preserved for
  `fields_attributes[][name]` sequencing); `avatar`/`header` file parts run
  the media pipeline (sniff allowlist jpeg/png/gif/webp, 8 MB caps vs
  `max_file_size`, decode check via `media::processing::process`), stage
  under the blob store, and persist references on the account row in the
  same `update_account_profile` transaction. Absent = unchanged,
  present-but-empty = clear to `missing.png` defaults; non-empty
  urlencoded/JSON avatar values 422 (a string is never an image).
  Failures use the file's existing 422 shape, never 415/500.
- `src/domain/account.rs`, `src/application/ports.rs`,
  `src/persistence/redb/accounts.rs` (+ `federation.rs` remote-row
  defaults) — `avatar_key/content_type/description`,
  `header_key/content_type/description` on the row; `AccountProfileUpdate`
  carries the slots (`ProfileImageData`); every profile write commits a
  `profile_update` federation event in the same transaction (no-op guard
  keeps remote rows silent). serde defaults keep old rows decoding (JSON
  codec).
- `src/activitypub/actor.rs` — served `Person` carries `icon`/`image`
  (`ActivityPub::ImageSerializer` shape: `type`/`mediaType`/`url` +
  `summary`) only while a blob is referenced.
- `src/api/mastodon/dto.rs`, `src/api/oauth/userinfo.rs` —
  `avatar(_static)`/`header(_static)` render the staged blob URL, else the
  `missing.png` assets; `picture` in userinfo follows.
- `src/activitypub/outgoing.rs`, `src/federation/events.rs` —
  `profile_update` event builds the full actor document through the same
  `into_json` the served endpoint uses and fans an `Update` to remote
  follower inboxes (immediate, no 5 s debounce; follower-only recipients —
  documented divergence).
- `tests/avatar_upload.rs` (pre-existing, 5 tests) — all pass.

Windows-only test fixes (zero behavior change on Linux, needed for green gates):

- `tests/round3_heic_tiff.rs`, `tests/phase7_1_transcode.rs` —
  `#![cfg(unix)]` on the whole file: both harness POSIX `sh` fake-ffmpeg
  scripts (`chmod +x`, `#!/bin/sh`); they can never execute on Windows.
  Previously `cargo check --all-targets` failed on
  `std::os::unix::fs::PermissionsExt`.
- `tests/secret_handling.rs` — `drop(store)` before reading
  `birchtree.redb` bytes back: redb holds a file lock while open, which
  Windows refuses to read through (OS error 33); Linux reads through fine.
  All writes are committed before the drop, so the assertion is unchanged.

Docs (in place, all already `Status: Draft`): `docs/endpoint-manifest.md`
(update_credentials row), `docs/compatibility-profile.md` (Remaining list),
`docs/TODOs.md` (follow-up marked shipped), `docs/phase-4-1-exit-criteria.md`
(multipart residual), `docs/application-ports.md` (profile slots + event),
`docs/federation-contract.md` (new "Profile update distribution" section).

## Verified (real output)

- `cargo check --all-targets` → exit 0 (one pre-existing warning:
  unused `pid` in `src/media/transcode.rs:731`).
- `cargo test --test avatar_upload` → 5 passed, 0 failed.
- Full `cargo test` → exit 0: 61 result lines ok, 0 FAILED,
  690 passed total (2 pre-existing ignores + unix-gated files empty).
- Toolchain note: baremetal had `~/.cargo` shims with a broken (partially
  installed) MSVC toolchain and no linker. Installed the GNU toolchain
  (`rustup toolchain install stable-x86_64-pc-windows-gnu`, rustc 1.99.1)
  plus WinLibs UCRT `gcc.exe` via winget for `cc-rs` (ring).
  `RUSTUP_TOOLCHAIN` unset; default is now
  `stable-x86_64-pc-windows-gnu`. Parent may want to pin or revert this
  machine-level default.

## Open questions (for parent)

- `scripts/*.sh` show `mode change 100755 => 100644` (exec bit lost).
  Left untouched (no-checkout rule) — parent should restore with
  `git checkout -- scripts/` or `chmod +x` on Linux if the bits matter.
- `src/media/transcode.rs:731` unused-`pid` warning predates this work;
  left as-is.
- The two `#![cfg(unix)]` gates and the `drop(store)` fix touch shared
  test files — owned by parent per AGENTS; review the 3 small diffs before
  committing.
- Rig-side follow-ups from the handoff (seeder avatar/banner upload from
  `/root/rig/assets`, pinetree `ap/show` remote check, Keystone client
  acceptance) need the Linux rig — not done here.
