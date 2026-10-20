# 2026-10-20 — outbound mail delivery (SMTP backend)

## What

Finished the email backend: real SMTP delivery through `lettre`, requested
resends flow through durable jobs (no network in the request path), config
drives everything (new `--smtp-url`/`--mail-from` flags + the JSON file keys
from the config slice).

## Changed

- `lettre` (sync transport, native-tls) added.
- `src/mailer/mod.rs`: `SmtpMailer` delivers for real
  (`smtp://` = STARTTLS except loopback, `smtps://` = implicit TLS, default
  ports 587/465, creds from URL userinfo, query strings refused);
  `MailEnv` is now config-driven `{from, enabled}` (+`from_config`).
- `src/application/ports.rs`: `MailSender::send_password_reset`.
- `src/mail/mod.rs`: `LoggingMailSender` gains the reset method;
  new `SmtpJobSender` bridge (mailer + from + base URL) for the worker.
- `src/domain/job.rs`: new `JobKind::SendPasswordResetEmail`
  (at-least-once documented).
- `src/jobs/handlers.rs`: reset arm; confirmation arm prefers an explicit
  payload token (resends) with the job-id derivation as fallback.
- Endpoints: resends validate, enqueue with a random 32-byte token each
  request, answer `200`; disabled → `503`; enqueue failure → `500`.
- `src/api/mastodon/mod.rs` + `router.rs`: `ApiState.mail_env` from server
  config (9 test construction sites take `MailEnv::disabled()`).
- `src/main.rs`: `--smtp-url=`/`--mail-from=` flags; worker `MailSender` is
  SMTP-backed when configured, logging otherwise.
- Docs: D12, manifest mail rows, `configuration.md` SMTP section.
- Tests: `tests/email_smtp.rs` (4: fake-sink delivery, handler delivery,
  endpoint→queue→worker end-to-end with random-token assertion,
  disabled-refusal with nothing queued).

## Verified

- `cargo check --all-targets` warning-free; `cargo test --release` green
  (no failing suite); `tests/email_smtp.rs` 4 passed against a fake SMTP
  sink (quoted-printable-aware assertions).

## Open (unchanged)

- Confirmation redemption (`email_verified` never flips), signup-app
  ownership, public forgot-password (email index), durable redb invite
  store, live-provider interop (only fake-sink tested).
