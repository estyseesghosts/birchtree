# 2026-10-22 — S9 config split (decomposition proposal)

## What changed
S9 per proposal: `src/config.rs` (1113 → ~790 lines) split into `src/config/`:
- `remote_cache.rs` (new) — `RemoteCacheMode` + `RemoteCacheConfig` verbatim.
- `transcode.rs` (new) — `Config::transcode_config` as a same-crate
  inherent impl, verbatim (knob fields stay on `Config`).
- `validate.rs` (new) — per-section validators with byte-identical error
  strings (62 literals verified present): `limits`, `transcode`, `feeds`
  (+license/place/alt-text), `federation`, `cache`, `gaps`, `relays`,
  `site_files`, `keyring`; orchestrator (9 lines) calls them in the EXACT
  original field order. 8 `#[cfg(test)]` tests assert exact strings.
- `src/config.rs` keeps struct + `Default` + `load*` + helpers + thin
  `Config::validate(&self)` delegating to `validate::validate(self)`;
  adds `pub mod` + re-exports.
- `src/config/feeds.rs` (agent-created duplicate of the feed-gate block)
  DELETED by parent: `validate.rs::validate_feeds` already subsumes it.

Parent fixes at merge (beyond the agents' files):
- First-error precedence: the agents grouped relay checks before cache
  checks and limits-tail checks first, changing precedence for
  multi-invalid configs. Re-split into 9 sections in original linear
  order (limits → transcode → feeds → federation → cache → gaps → relays
  → site_files → keyring). Single-fault messages identical; precedence
  now identical too.
- `feeds.rs` removal (above).

Delegation: 2 subagents (sections vs validate, disjoint outputs);
parent reconciled the overlap, cut over, and ordered the sections.
`rustfmt` clean (file-scoped).

## Verification (native Windows, shared target)
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test config`: 31 passed incl. 8 new exact-string tests.
- `cargo test` (full): 1153 passed, 0 failed.
