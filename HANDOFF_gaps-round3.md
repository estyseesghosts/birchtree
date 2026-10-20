# Handoff: Birchtree — round 3 (invites+email, HEIC/TIFF, notification requests, encrypted push, remote timeline, admin stats)

## Context (read first)

- Plan: `birchtree.md` §7 (your phase exit gates) + §5 (media/transcode, push, search). Architecture rules: `AGENTS.md` — dependency direction `domain` ← `application` ← adapters, one txn commits state+indexes+jobs, network/ffmpeg/mail after commit in idempotent jobs.
- Gap register: `docs/compatibility-profile.md` (D12 email/invites, D4 images, D20 notification-requests, D42 push, D19 remote, D31/D39 stats). Every deliberate difference gets a row; an unregistered difference is a bug.
- Contract: `docs/endpoint-manifest.md` (224/226 — keep it so; flip any row your work affects).
- Reference: Mastodon v4.7.3 pinned (`8ff11c2b`), read-only (`~/fedifedifedi/mastodon`, `pinetree` symlink). Cite `file:line` both sides for behavior claims.
- Starting state: `master` at `f9c2c7c` plus a LARGE uncommitted round-2 tree (~70 files, D15/D21/D24/D36-D40, tombstones, cooldowns). Run `git status --short` first, respect others' uncommitted work, never touch it.
- Evidence discipline: wire/code evidence before behavior changes; executed tests, not readings; `docs/logs/` entry + dated `state/logs/YYYY-MM-DD-<topic>.md` (append-only, never rewrite).

## Objective

Close the six user/admin gaps below. Each ships with tests + register/manifest updates. No success-shaped stubs: unbuilt stays absent (404/422), refused keys stay refused with reasons, no fabricated mail/score/telemetry.

## Subagent delegation (HARD RULE per AGENTS.md)

Subagents share one worktree — partition so they cannot collide:
- Disjoint file sets per subagent (listed below). Shared files (`docs/*`, router/ports, schema/migrations, shared test files) are owned by PARENT for merging — subagents propose patch snippets only, never edit them directly.
- Subagents use uniquely-named test files (`tests/round3_<area>.rs`), leave changes uncommitted, never `git commit`/`stash`/`reset`/`checkout --` anything.
- Parent merges, updates `docs/*`, runs the gate.

## Work split (5 subagents, parallel)

- **A — invites + email** (`src/domain/user.rs`, `src/application/accounts.rs`, `src/api/mastodon/endpoints/accounts.rs`, `src/config.rs`, NEW `src/mailer/*`): invite-token create/accept/redeem, confirmation-mail + password-reset-mail send paths (pluggable SMTP/backend; no-send without config = explicit disabled state, never silent success). Verify against upstream `registrations_controller` / `confirmation` flows. Tests: `tests/round3_invites_email.rs`.
- **B — HEIC/TIFF → WebP** (`src/media/ingest.rs` sniffer, `src/media/transcode.rs` plan+job, `src/media/processing.rs`): accept `image/heic|heif|tiff`, route to `ImageToWebp`, fixture tests per §7 exit (real HEIC/TIFF fixtures → WebP). Decoder caps via existing toolchain validation. Tests: `tests/round3_heic_tiff.rs`. Proposes D4-register snippet only.
- **C — notification requests + encrypted push** (`src/application/notifications.rs`, `src/domain/notification*.rs`, NEW push-crypto module, `src/api/mastodon/endpoints/notifications*.rs`, `src/api/mastodon/endpoints/web.rs`): filter → request-queue → merge → `notifications_merged` event pipeline (closes D20 properly); Web Push aes128gcm encrypted payload per RFC 8291 (replaces/complements VAPID tickle D42 — verify upstream payload shape first). Tests: `tests/round3_notif_push.rs`.
- **D — remote public timeline** (`src/application/timelines.rs`, `src/streaming/fanout.rs`, `src/api/mastodon/endpoints/timelines.rs`): VERIFY FIRST against `fanout.rs` `Locality::Remote` — D19 row claims empty but federation landed. If already working, prove with a test + propose D19-close snippet; only then fill any real hole (remote ingest → public:remote + media variant). Tests: `tests/round3_remote_timeline.rs`.
- **E — admin stats completeness** (`src/application/admin.rs`, `src/application/trends.rs`, `src/domain/admin_infra.rs`, `src/persistence/redb/admin*.rs`, `src/api/mastodon/endpoints/admin_*.rs`, `src/api/mastodon/endpoints/trends.rs`): NodeInfo `usage.users`+`usage.localPosts` (local counts); D39 leftovers — `interactions` event log OR keep-refused with reason, tag `accounts/uses/servers` + dimensions from 7-day history, `software_versions` census OR keep-refused, sign-in-cohort retention, full `space_usage` (media+db). Each key served or refused-with-reason, never zero-filled. Tests: `tests/round3_admin_stats.rs`.

Parent synthesizes: A×E (approval states vs invites), C×D (notification filtering vs remote), B×E (`space_usage` grows with new conversions).

## Collision rule

Round-2 uncommitted work owns: `src/activitypub/wire.rs`, `src/application/federation_inbound.rs`, `src/application/statuses.rs`, `src/persistence/redb/statuses.rs`, `src/federation/*`, `src/jobs/account_deletion.rs`, `tests/phase7_gaps_round2_*.rs`, `tests/phase6_gaps_round2_e.rs`. Do not edit those paths. If a shared file must change, coordinate via parent.

## Implementation rules

- Domain knows no HTTP/DB shapes; REST + ActivityPub call the same application use cases.
- Mail/send/push/crawl/fetch happen AFTER commit, in bounded idempotent jobs → quarantine, never inline.
- Federation input untrusted: size/timeout/redirect/SSRF caps on every new fetch (link crawl, remote docs).
- No PII leaks in logs; tokens/secrets never logged.
- Update proposals for `docs/endpoint-manifest.md` + `docs/compatibility-profile.md` (D4/D12/D19/D20/D31/D39/D42) as snippets to parent. A gap closed without its register row does not count.
- Gate: `cargo check --all-targets` warning-free + `cargo test --release` green + HTTP smoke for behavior changes (200/401/403/422 shapes).

## Deliverables

- Invites + confirmation/reset mail (or explicit disabled-state), HEIC/TIFF→WebP fixtures green, requests pipeline + `notifications_merged`, encrypted push payloads, remote timeline proven/closed, every admin key served-or-refused-with-reason.
- Each gap closed with test evidence or explicitly remaining with blocking dependency named.

## Non-goals

- Search stays deploy-time restart (`--search-mode`, D6) — document, do not build a runtime toggle.
- Do not touch round-2 tag/transcode/search/conversations code, do not run §8 item 11 parity suite, do not push or commit.
