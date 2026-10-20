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

## File-only keys (no CLI flag)

These can only be set via the file: `max_file_size`,
`worker_poll_interval_ms`, `trends_enabled`, `invites_enabled`,
`peers_api_enabled` (default true), `activity_api_enabled` (default true),
`domain_blocks_public` (default false, upstream default hidden),
`local_live_feed_access` / `remote_live_feed_access` (default `public`;
`authenticated` or `disabled`, upstream `FEED_ACCESS_MODES`),
`local_topic_feed_access` (default `public`; `public`/`authenticated`) and
`remote_topic_feed_access` (default `public`; `public`/`authenticated`/
`disabled`). Non-`public` feed gates require login on the public, tag and
link timelines (anonymous → 401); `disabled` serves `[]` to non-staff.
Every other key has both a file key and a CLI flag (`src/main.rs:42-169`):
`--public-url`, `--data-dir`, `--listen`, `--trusted-proxy` (repeatable),
`--max-body-size`, `--max-status-chars`, `--site-title`, `--site-description`,
`--site-contact-email`, `--site-contact-username`, `--streaming-url`,
`--federation-allow-http`, `--federation-authorized-fetch`,
`--federation-extra-ca`, `--federation-allow-private`, `--debug-weak-rate-limits` /
`--rate-limit-multiplier`, `--registration-open` / `--registration-closed`,
`--registration-require-approval`, `--search-mode`, `--no-transcode`,
`--ffmpeg-path`, `--webp-quality`, `--vp9-crf`, `--vp9-cpu-used`,
`--opus-bitrate-kbps`, `--transcode-wall-time-secs`,
`--transcode-max-output-bytes`, `--transcode-max-memory-bytes`,
`--vapid-public-key`, `--vapid-private-key`, `--smtp-url`, `--mail-from`,
`--web-client-dir`.

## Reloading

Configuration loads once at startup; changing the file or flags requires a
restart. There is no runtime/admin toggle for any key (matches upstream:
Mastodon has no runtime backend switches either; see D6).

## Core extension policies (Stage 1; default off, Mastodon parity when off)

Three-state server-admin policies, set via JSON file keys with CLI/env
override per the existing config pattern:

- `license_policy: "off" | "unenforced" | "enforced"` (default `"off"`).
  `off` = disabled: license params accepted-and-ignored (stored NULL),
  wire omits `license` / `preferredLicense`. `unenforced` = enabled,
  optional (round-trip local + wire, 422 on out-of-enum). `enforced` =
  enabled, mandatory: effective license (per-attachment else author
  default) must be non-NULL on top-level non-DM statuses with media,
  else Mastodon-compatible 422. Replies and DMs exempt. Media upload
  alone never 422s for licenses.
- `place_policy: "off" | "unenforced" | "enforced"` (default `"off"`).
  `off` = disabled: `place_id` accepted-and-ignored (stored NULL), wire
  omits `location`. `unenforced` = enabled, optional (unknown place_id
  → existing 422 path; missing place allowed). `enforced` = enabled,
  mandatory: top-level non-DM statuses require a resolvable place_id,
  else 422. Replies and DMs exempt. No proximity/discover/geo-search.
- `alt_text_policy: "off" | "nag" | "block"` (default `"off"`). Local
  writes only; federation ingest never drops for missing alt-text.
  `off` = no behavior change. `nag` = allow upload/post but return a
  documented nag signal (one signal, pinned in endpoint-manifest +
  tested, same on v1/v2 media + statuses). `block` = 422 when an
  image/video attachment is left without a description (media v1/v2
  POST, status create/edit). Text-only posts unaffected.
