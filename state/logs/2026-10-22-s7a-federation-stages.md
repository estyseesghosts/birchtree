# 2026-10-22 — S7a federation stages (decomposition proposal, part 1 of 2)

## What changed
New stage modules (wired into `federation/mod.rs`, compiling, pub API —
orchestrator rewires are S7b, explicitly pending):
- `federation/media_fetch.rs` (315 lines): `parse_job`, `load_source`,
  `gate_url`, `fast_path_if_cached` (`Hit`/`Miss`), `signed_get`,
  `enforce_declared_cap`, `chunk_over_cap`, `commit_bytes` (delegates to
  `settle_staged`, no logic fork). Constants byte-identical
  (`MAX_REMOTE_MEDIA_BYTES` 40 MiB, `MAX_REDIRECT_HOPS` 5).
- `federation/events_build.rs` (386): 5 builders (each <80 lines),
  verbatim arms; private helpers duplicated verbatim (dedup to
  `pub(crate)` in one place is S7b material).
- `federation/delivery_send.rs` (88): `classify`, `cooldown_host`,
  `charge_budget`, `enqueue_deferred`. Constants referenced (not
  redefined): `DEST_FAILURE_BUDGET` 10, `DEST_MAX_DEFERRALS` 25.
- `federation/threads_auth.rs` (284): `QuoteAuthCtx` + `authorize_step()`
  iterator; hop/budget constants unchanged (8/100/40).
- One parent fix: `enqueue_deferred` now ends `?; Ok(())` (was returning
  the `enqueue_at` `Result<Job>` against a `Result<()>` signature).

## Why S7b is separate
`fetch` (340 lines), `events::run` (370), `delivery::run` (216) and
`check_authorization_remote` rewires are behavior-critical async paths;
they deserve isolated diffs + federation interop runs, not a batch merge.
The stages above are verbatim and compile-checked; the originals are
untouched, so this commit changes no runtime behavior at all.

## Verification
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test` (full): 1153 passed, 0 failed.
- `rustfmt --edition 2024 --check`: clean.
