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

## Outbound mail (SMTP)

`POST /api/v1/emails/confirmations` and `POST /api/v1/emails/password_reset`
deliver for real once a provider is configured (flags or file):

```sh
birchtree --smtp-url=smtps://user:pass@mail.example.com:465 \
  --mail-from=noreply@example.com
```

Shapes: `smtp://[user:pass@]host[:port]` (STARTTLS required, except on
loopback for labs) and `smtps://…` (implicit TLS, default port 465).
Resend endpoints enqueue durable jobs and answer `200 {}` immediately; the
worker delivers after commit (at-least-once: a crash retry can duplicate a
message). Without a provider the endpoints answer `503` and registration
mails only reach the log. Mail bodies/subjects mirror upstream
`UserMailer` (Devise defaults); token links point at the public URL.

## File-only keys

These have no CLI flag and can only be set via the file: `max_file_size`,
`worker_poll_interval_ms`, `trends_enabled`, `invites_enabled`,
`peers_api_enabled` (default true), `activity_api_enabled` (default true),
`domain_blocks_public` (default false, upstream default hidden).

## Reloading

Configuration loads once at startup; changing the file or flags requires a
restart. There is no runtime/admin toggle for any key (matches upstream:
Mastodon has no runtime backend switches either; see D6).
