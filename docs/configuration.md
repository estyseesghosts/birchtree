# docs/configuration.md

Status: Draft

How to configure the birchtree server. Source of truth for key names and
units: `docs/config-schema.json` (pinned to `src/config.rs` by
`tests/config_file.rs`, which fails if the two drift).

## Precedence

Defaults → `--config=PATH` JSON file → CLI flags. `validate()` runs once
over the merged result; any error is fatal and names the offending key.

```sh
birchtree --config=/etc/birchtree/config.json --listen=0.0.0.0:3000 serve
```

Every key is optional in the file; absent keys take compiled defaults.
Unknown keys are rejected (typo safety). A minimal file is at
`config.example.json` in the repo root.

## Secrets

`smtp_url` and `vapid_private_key` accept an `env:VAR_NAME`
indirection so operators need not store secrets in the file:

```json
{ "smtp_url": "env:BIRCHTREE_SMTP_URL", "mail_from": "noreply@example.social" }
```

An unset variable is a startup error naming the variable. Secret values are
never logged.

## File-only keys

These have no CLI flag and can only be set via the file: `max_file_size`,
`worker_poll_interval_ms`, `trends_enabled`, `invites_enabled`, `smtp_url`,
`mail_from`.

## Reloading

Configuration loads once at startup; changing the file or flags requires a
restart. There is no runtime/admin toggle for any key (matches upstream:
Mastodon has no runtime backend switches either; see D6).
