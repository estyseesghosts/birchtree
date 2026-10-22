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

## Deployment mode

`mode` is `production` (default) or `lab` (CLI: `--lab`). Production
refuses to start with weakened settings — `federation_allow_http`,
`federation_allowlist`, `federation_extra_ca`, or
`rate_limit_multiplier > 1` — unless the exact override phrase is set
(CLI: `--unsafe-production-override=I-ACCEPT-WEAKENED-SECURITY-IN-PRODUCTION`).
The refusal names the offending keys and exits 2; the override is logged at
warn level. `check_runtime_mode()` runs after `validate()`; the secret-free
startup summary (`Config::startup_summary`) is logged on every boot and
never contains credentials, tokens, keys or seeds.

## Client IP and trusted proxies

Rate limiting and IP blocks resolve the client IP from the TCP peer plus
`X-Forwarded-For` only when the peer is a configured `--trusted-proxy`
(repeatable). The chain rule is rightmost-untrusted: starting from the
peer, the resolver walks the forwarded chain back through trusted proxies
and takes the first untrusted address (see
`rate_limits::resolve_client_ip`). An untrusted peer's `X-Forwarded-For`
is ignored entirely, so it cannot spoof its rate-limit identity. The
limiter is bounded with expiry/eviction (no clear-all), with independent
thresholds on login, token issuance, registration, password-reset and
confirmation-resend flows, plus per-account/address controls on reset that
do not reveal whether an account exists.

## Secret keyring

TOTP seeds, local ActivityPub signing keys and mail-outbox bearers are
sealed with AES-256-GCM under an operator-managed keyring that lives
outside the database (file or environment variable, never the config
file's inline values):

```json
{ "secret_keyring_file": "/etc/birchtree/keyring" }
```

or `{ "secret_keyring_env": "BIRCHTREE_KEYRING" }` (CLI:
`--secret-keyring-file=` / `--secret-keyring-env=`). The format is one
`<key id>:<64 hex chars>` per line; blank lines and `#` comments are
ignored; the first key seals, the rest only open older values (rotation =
add the new key first, re-seal, then retire the old one). Keyring files
must not be group/other-readable on Unix. Production refuses to start
when no key loads; lab starts with a warning and plaintext secrets.
Losing every key that sealed a row makes that row **unrecoverable** — it
is not a migration problem: MFA enrollments cannot be decrypted, actor
keys cannot be recovered (fail closed, never silently re-generated), and
sealed outbox bearers cannot be mailed. Back the keyring up separately
from the database.

## Cache and media policy keys

`remote_cache` budgets the remote-media cache (`mode`: `on_demand`
default, `warm`, or `disabled`; per-class caps plus a global cap;
`avatar_retention_days`; scoped `warming` reuse of `FetchRemoteMedia`,
relay-excluded). On demand is the effective policy: ingest records
metadata only and enqueues no fetch unless the policy opts into warming;
bytes arrive on the first cache miss through a deduplicated
single-flight job. `durable_dir` (default `<data_dir>/media`) and
`cache_dir` (default `<data_dir>/media_cache`) must be disjoint trees
that exclude the database file (resolved through symlinks; startup
refuses overlaps). `max_file_size` is enforced per file independently of
`max_body_size`; uploads stream to disk, responses stream with byte
ranges, and decode/ffmpeg work runs off the async runtime under bounded
concurrency.

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
`--web-client-dir`, `--lab`, `--unsafe-production-override`,
`--secret-keyring-file`, `--secret-keyring-env`.

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
