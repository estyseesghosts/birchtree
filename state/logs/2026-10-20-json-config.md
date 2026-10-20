# 2026-10-20 — JSON config file support

## What

`--config=PATH` loads a JSON file over compiled defaults; CLI flags
override file values; `validate()` runs once over the merge. Unknown keys,
unreadable files and malformed values are fatal startup errors naming the
key/path. `env:VAR` indirection for `smtp_url` / `vapid_private_key`.
`docs/config-schema.json` pinned to `src/config.rs` by test (drift = red).

## Changed

- `src/config.rs`: serde Deserialize+Serialize on `Config`
  (`deny_unknown_fields`, per-field defaults), millis encoding for
  `worker_poll_interval` (key `worker_poll_interval_ms`), `load_from_file`,
  `resolve_env_indirect`.
- `src/search/mod.rs`: `SearchMode` serde (lowercase).
- `src/federation/policy.rs`: `Cidr` Display + serde (string form).
- `src/main.rs`: `config_base()` pre-scan + `--config=` skip arm +
  `resolve_env_indirect()` before `validate()`.
- `docs/config-schema.json`, `docs/configuration.md`, `docs/README.md`
  index rows, root `config.example.json`.
- File-only keys unlocked (no CLI flag needed): `max_file_size`,
  `worker_poll_interval_ms`, `trends_enabled`, `invites_enabled`,
  `smtp_url`, `mail_from`.

## Verified

- `tests/config_file.rs` (6 passed): overlay, typo rejection, malformed
  values (parse vs validate), missing-file naming, env indirection,
  schema↔struct parity.
- `cargo check --all-targets` warning-free; live binary: valid file +
  flag override runs, missing file and typo key error as shown.
