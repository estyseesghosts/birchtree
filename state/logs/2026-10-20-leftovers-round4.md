# 2026-10-20 — leftovers round 4 (D12/D39/D42 remainders)

## What

Parent pre-built the shared base (schema v12: INVITES/EMAIL_INDEX/
EMAIL_TOKENS/INTERACTION_EVENTS + email backfill; ports: account_by_email,
token store/redeem, email_verified, InteractionLog; domain: EmailToken,
InteractionEvent, created_by_application), then 3 parallel subagents on
disjoint files, then parent merged + fixed a critical push-KDF bug the
audit found.

## Changed

- D12 (subagent A + parent): redb InviteStore (single-txn redeem);
  ApiState.invites threaded (memory default in tests, redb in prod);
  account_by_email + EMAIL_INDEX (created + backfilled, duplicates
  refused); token store/consume; confirm_email + ownership (grandfather
  rule); `GET /auth/confirmation` page; public forgot-password
  (always-200); registration-job token persistence (handler stores
  before sending); duplicate-email Conflict; reset TTL 6h (Devise
  parity), confirmations never expire. Remains: password-reset
  redemption HTTP flow.
- D39 (subagent B + parent): five verified upstream call sites journal
  (non-self replies, created favourites/reblogs, votes, new follows;
  `Post` kind deleted as wrong); redb journal + bounded counts;
  `interactions` measure served; software_versions kept refused with
  the exact probe citation. Old absence pins updated (3 files).
- D42 (subagent C + parent KDF fix): legacy `aesgcm` with upstream
  selection; **round-3 `aes128gcm` was single-stage and undecryptable —
  fixed to the two-stage §3.4 form, byte-locked by the RFC Appendix A
  intermediates + §5 example decrypt** (`tests/round3_notif_push.rs`).
  Remains: live-service interop.
- Docs: D12/D39/D42 rows, manifest (measures, push, invite/email,
  +`/auth/confirmation`), fix-plan P3 decisions recorded earlier.
- New tests: round4_email_confirm (13), round4_metrics (7),
  round4_push_aesgcm (7), +2 RFC vector tests in round3_notif_push.

## Verified

- `cargo check --all-targets` warning-free; `cargo test --release`
  (full gate below) green.

## Open

- Reset-redemption flow, live interop proofs (push/SMTP/federation lab),
  D35 roadmap, fix-plan P0–P2 (reblog default, block-gates, stubs
  build, triage queue).
