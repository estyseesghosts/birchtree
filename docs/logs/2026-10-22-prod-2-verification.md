# 2026-10-22 prod-2 verification

Gate per `docs/birchtree-prod-2.md` item 12: `cargo test`, `cargo audit`,
release build, and smoke run on one commit. Product commits: `a0ae228`
(test harnesses: mail-tolerant drains + lab-loopback push) on top of
`2a42640` (prod-2 items 1–12), plus `e101dc7` (smoke/lab keyring only —
no `src/` change, so test/release evidence carries over).

## `cargo test --no-fail-fast`: EXIT 0, 0 failed
Full debug suite green (`C:\Users\julie\AppData\Local\Temp\opencode\prod2-full-test.log`).
Every `test result:` line reports `0 failed`; grep for `[1-9][0-9]* failed`
finds no matches. Includes the 9 previously-red targets, fixed by `a0ae228`:
`c_media_http` (10), `fix4_p2_accounts` (11), `fix4_p2_fedpush` (14),
`phase4_notifications` (4), `phase4_scheduled` (3),
`phase7_3_annual_reports` (3), `phase7_conversations_push` (9),
`round3_notif_push` (8), `round4_push_aesgcm` (7).

## `cargo check --all-targets`: clean (warnings only, pre-existing)
Finished in 1m44s. Warnings are pre-existing and unrelated to prod-2:
`src/media/transcode.rs:742` unused `pid`, `src/media/remote_cache.rs:831`
`full_entry` dead code, `tests/fix_emoji_cache.rs` unused imports.

## `cargo build --release`: EXIT 0
(`C:\Users\julie\AppData\Local\Temp\opencode\prod2-release.log`.)

## `cargo audit`: dependencies show the fixed state
`cargo audit` interactive run was inconclusive in this environment
(network fetch stalls), but the tree state pins the disposition recorded in
`775a463`: `Cargo.lock` has `blurhash 0.2.3` (RUSTSEC-2023-0083 fixed by
upgrade) and no `name = "rsa"` crate (no RustCrypto `rsa` in tree).

## Smoke (fail-closed pass path, `e101dc7`)
`smoke-phase*.sh` + lab launchers mint a throwaway `0600` keyring
(`1:0…0`) and pass `--secret-keyring-file`, with dummy
`--smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social`
(from `2a42640`), exercising the production fail-closed guards' pass path:
- `smoke-phase1.sh`: EXIT 0
- `smoke-phase2.sh`: EXIT 0
- `smoke-phase3.sh`: EXIT 0
- `smoke-phase4.sh`: EXIT 0
- `smoke-phase5.sh`: EXIT 0
- `smoke-phase6.sh`: EXIT 0
- `smoke-phase7.sh`: EXIT 0 (log path fixed to `$DATA_DIR/phase7.log`: the
  script previously wrote to `/tmp/opencode/phase7.log`, which does not exist
  on this host — environmental, not product)
- `smoke-phase7-1/7-2/7-3.sh` stubs: all `*_OK`, EXIT 0
