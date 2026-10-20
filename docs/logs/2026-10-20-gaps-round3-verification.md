# 2026-10-20 — gaps round 3 verification (HANDOFF_gaps-round3.md)

Parent synthesis of 5 parallel subagents (disjoint file sets, uniquely-named
test files, parent-owned shared files merged by hand).

## Area results

- **A invites+email** (`src/domain/user.rs`, `src/application/accounts.rs`,
  `src/api/mastodon/endpoints/accounts.rs`, `src/config.rs`, new
  `src/mailer/`): invite create/accept/redeem + confirmation/reset send paths
  behind a pluggable backend (`LogMailer` default, `SmtpMailer` validated stub
  refusing loudly); unconfigured SMTP = explicit 503, never fake success.
  Parent wiring: `pub mod mailer` (`src/lib.rs`), 4 routes + `InviteStore` /
  `MailEnv::disabled()` Extension layers (`src/api/mastodon/router.rs`).
  Store is in-memory (durable redb follow-up); `email_verified` never flips;
  no public forgot-password (needs email index). Evidence:
  `tests/round3_invites_email.rs` (16 passed).
- **B HEIC/TIFF→WebP** (`src/media/ingest.rs`, `transcode.rs`,
  `processing.rs`): `image/heic|heif|tiff|x-tiff` accepted → `ImageToWebp`;
  undecodable → `Failed`/422. Superseded pin in
  `tests/phase7_1_transcode.rs:355-357` replaced. No ffmpeg in this env, so
  true HEIC→WebP bytes need a verifying machine with hevc/heif decode.
  Evidence: `tests/round3_heic_tiff.rs` (7 passed).
- **C notification requests + encrypted push** (`src/application/notifications.rs`,
  `src/domain/notification.rs`, new `src/application/push_crypto.rs`,
  `src/application/push.rs`, `src/api/mastodon/endpoints/notifications.rs`,
  6 router lines): filter→request→merge pipeline; `notifications_merged`
  (`"1"`) only on real merges; RFC 8291 `aes128gcm` for standard subs
  (`ring`-only, zero new deps), tickle preserved for legacy.
  One legitimate interaction fixed by parent: upstream-faithful
  `private_mentions=filter` default filtered the legacy push test's
  stranger-DM mention → test now opts out via the policy endpoint.
  Evidence: `tests/round3_notif_push.rs` (6 passed).
- **D remote timeline**: VERIFIED already working, no source changes.
  Remote ingest → public-but-not-local index; `remote=true`/`only_media`
  serve; `public:remote[:media]` streams emit; shared visibility gates.
  Evidence: `tests/round3_remote_timeline.rs` (5 passed).
- **E admin stats** (`src/application/admin.rs` area,
  `src/domain/admin_infra.rs`, `src/persistence/redb/admin_infra.rs`,
  `src/api/mastodon/endpoints/admin_infra.rs`/`instance.rs`,
  `src/activitypub/http.rs` NodeInfo usage): tag measures/dims with id rule,
  sign-in-unioned retention, media+measured-redb `space_usage`;
  `interactions`/`software_versions` refused with reason constants.
  Parent follow-through: v2 instance `usage.users.active_month` from the
  sign-in log (`src/api/mastodon/dto.rs` + `instance.rs`). Evidence:
  `tests/round3_admin_stats.rs` (6 passed). Dashboard table:
  `HANDOFF_round3_admin-dashboard-note.md` (filled, kept in tree).

## Parent extras

- `tests/round3_parent_smoke.rs` (3 passed): invite 404 shape, email
  confirmation explicit 503, policy/requests routes wired + defaults.
- Syntheses: A×E staff-invite approval bypass (in D12); C×D shared
  public-feed gates (in D19); B×E conversions land in measured `space_usage`
  automatically (derived blobs counted as media bytes).

## Gate

- `cargo check --all-targets`: 0 warnings, 0 errors.
- `cargo test --release`: 3 consecutive full runs green (588+ passed,
  0 failed); one earlier full run showed a single transient failure that did
  not reproduce in 3 reruns (unidentified flake, no FAILED section captured).
- Manifest grew by ~17 rows (new invite/email/request/policy routes);
  register flips: D4 extended, D12 narrowed, D19 closed, D20 implemented,
  D31 implemented, D39 extended, D42 partly implemented.

## Open (blocking deps named)

- Durable redb `InviteStore`; real SMTP transport crate + secrets handling;
  confirmation redemption flow + `created_by_application` ownership (schema);
  public forgot-password (email index); true HEIC→WebP byte proof machine;
  vanilla-admin-UI render check (no UI in env); live-push-service interop.
