# 2026-10-17 — Phase 6 verification (federation on the `activitypub_federation` crate)

Append-only evidence. Session log: `state/logs/2026-10-17-phase-6-federation.md` (Session 2). Gate: `docs/phase-6-exit-criteria.md` (In progress; Phase 6 is not complete).

Dates in the evidence are UTC. The runs took place on 2026-10-17 late and 2026-10-18 early (UTC).

## 1. Interop with vanilla Mastodon 4.7.3 (final run)

Command: `LAB=/root/parity/lab BT_DATA=/root/parity/birchtree-lab-data bash scripts/lab/phase6-interop.sh`
Output: `docs/logs/2026-10-17-phase-6-interop-run.out`. birchtree's federation log for the same run: `docs/logs/2026-10-17-phase-6-interop-birchtree.log` (escape codes stripped; `RUST_LOG=info,birchtree::federation=debug,birchtree::jobs::worker=debug,activitypub_federation=debug`).

Mastodon: `v4.7.3+birchtree-parity`, built from tag `8ff11c2b` (`SOURCE_COMMIT=8ff11c2b`), production env, `ALLOWED_PRIVATE_ADDRESSES=172.28.0.0/24`. birchtree: the final binary, `--federation-allow-private=172.28.0.0/24 --federation-allow-http --federation-extra-ca=<lab CA>`.

| # | Check | Result |
|---|---|---|
| 1 | Mastodon resolves `bob@birchtree.lab` (WebFinger and actor document) | PASS |
| 2 | Inbound Follow, Mastodon `alice` → birchtree `bob`: applied, Accept sent, Mastodon shows `alice` following | PASS |
| 3 | Outbound Follow, `bob` → `alice` (resolved by search `resolve=true`): delivered, Accept applied, birchtree shows following | PASS |
| 4 | Outbound Undo, `bob` unfollows `alice`: Mastodon removes `bob` from `alice`'s followers | PASS |
| 5 | Inbound Undo, Mastodon `alice` unfollows `bob`: applied, birchtree shows no follower | PASS |
| 6 | Pending follow on a locked account: `requested` and not `following`, then Mastodon's Accept applied | PASS |
| 7 | Mastodon rejects a pending follow: Reject applied, birchtree shows neither `following` nor `requested` | PASS |

**14 passed, 0 failed** (`RESULT: 14 passed, 0 failed`, `exit=0`).

Outcomes in birchtree's log for this run: Follow applied ×1; Accept applied ×2; Undo applied ×1; Reject applied ×1; Undo ignored (no follow to undo) ×1 (Mastodon's reset Undo for a follow birchtree never had, which is correct).

Signatures: the crate verified every inbound Mastodon signature (`verified signature for /users/bob/inbox`). The crate fetched Mastodon's keys over the lab TLS proxy.

## 2. The bugs the interop runs found

Each was found by a failing step, fixed, and covered (where it can be covered in process).

1. **Undo ID reuse (D33).** Mastodon reuses an Undo's ID (`…#follows/N/undo`) when it undoes the same follow row again. An Undo with no effect was recorded as seen, so the real Undo was reported as a duplicate, and `followed_by` stayed true. Fix: `commit_inbound` rolls back an `Ignored` outcome, dedup record included. Test: `an_ignored_undo_is_not_recorded_so_a_later_real_undo_applies`.
2. **Accept verify checked the wrong field.** It compared the embedded follow's actor (birchtree's follower) with the sender. It must compare the embedded follow's object (the followed account) with the sender. The 403 this caused is not retried by Mastodon, so the follow would have stayed pending for good. Tests: `accept_naming_another_followed_account_is_refused`.
3. **Local lookups on non-default ports.** Accounts keep the host without the port (`host_from_url`, an existing and tested decision). The crate's domain keeps the port because its `is_local` check compares `host:port`. Local lookups now use the account's domain. Smoke: `scripts/smoke-phase6.sh` on port 38932.
4. **The keygen stall (the largest).** A local actor's RSA key was generated on the first request that needed it. In this debug build that took seconds. Mastodon gives up at 10 s, and its retry is the only reason the follow eventually succeeded.
   - Before (fresh database, same lab steps): inbound Undo handled in **4.7 s** (`03:46:36.707` received → `03:46:41.445` handled); the first Follow was not handled until **~17 s** after it arrived (Mastodon's proxy logged `499`, Sidekiq `elapsed=10.028`, `fail`), then succeeded on retry.
   - Fix: the `user_created` job now creates the key, in a detached task so the worker is not held up (a worker that waited on it broke the phase 4 media smoke, which polls for 5 s).
   - After (fresh database, no warm-up): the key job logged at startup; inbound Undo handled in **1.5 ms** (`04.418` → `04.420`); inbound Follow in **12 ms** (`11.912` → `11.925`); Mastodon's delivery completed in **43 ms**; `following=true`.
   - The lazy path stays as a fallback for an account whose job has not run.
5. **TLS trust for federation.** The crate's client uses the platform's trust store, and OpenSSL does not read `SSL_CERT_FILE` on its own, so an environment variable could not work. Fix: `--federation-extra-ca=PEM`, a lab-only addition to the client's roots. The host trust store was not changed.
6. **WebFinger over plain HTTP in debug mode.** The crate's debug mode resolves WebFinger with `http://`. Our policy refuses plain HTTP unless `--federation-allow-http` is set (lab only). Refusal was correct; the lab sets the flag.

Earlier interop runs, before fixes 1–4: 11 of 13 checks, then 14 of 16. Their failing steps are the two items above (a stale database in the harness, which is also fixed in `scripts/lab/phase6-interop.sh`, and the keygen stall). Their outputs are not kept as evidence; the final run is.

## 3. In-process tests

- `cargo test --test phase6_federation`: **16 passed** (`tests/phase6_federation.rs`): discovery (WebFinger, actor document, NodeInfo), the refusal of an unsigned activity from a known actor with no state change, the personal inbox for an unknown account, the verify rules (Accept and Undo), the Phase 6 parse set (`Like`, `Announce`, `Create`, `Flag`, `Update`, `Delete`, `Question` do not parse), the Undo regression, the Accept from an account not followed, a redelivered applied Follow, and the delivery settlement (503 retries, 202 completes, 400 is final, a refused URL and a keyless sender quarantine).
- `cargo test --lib activitypub`: 3 passed (the error-status table).

## 4. Full gate

- `cargo check --all-targets`: zero warnings.
- `bash scripts/smoke-all.sh`: **`smoke-all: green`**. Log: `docs/logs/2026-10-17-phase-6-smoke-all.log`. Tests: **325 passed, 0 failed**, zero warnings. Phase 2, 3, 4, 5 and 6 smokes each printed `SMOKE_PHASE<n>_OK`.

Before this gate: `cargo test` after the axum 0.7 migration had 290 passing with two key-order tests fixed (sorted key lists, because the crate enables `serde_json/preserve_order`) and four more tests with the same cause in phase 4 (see the git diff of `tests/`).

## 5. What this does not show

- Only the Follow family is covered: Follow, Accept, Reject and Undo of Follow. Create/Note, Update, Delete, Like, Announce, Block, quote Request/Accept/Reject/Revoke and Flag are not implemented, so they are not verified.
- No thread completion, no signed or authorized fetch, no `outbox`, `followers` or `following` collections.
- The parity suite (§8 item 11) has not been run.
- The lab uses the crate's debug mode (its private-address and http switches), which is a lab-only setting. The production code path (`debug = false`) is covered by the in-process tests only, not by interop.
- Performance: not measured.
- Mastodon's `alice` was created with `save!(validate: false)` (the email domain check needs a real MX record). Her approval was done directly.
- The 4xx rule: the crate reports any 4xx (other than 408/429) as delivered. The log line says so; the remote's status is not recorded (D28).

## 6. Reproduction

1. Bring up the lab: `scripts/lab/README.md` (network, Postgres, Redis, Mastodon web and Sidekiq, two nginx proxies, `alice` on Mastodon, `bob` on birchtree).
2. `LAB=… BT_DATA=… bash scripts/lab/phase6-interop.sh`.
3. The per-step PASS/FAIL lines and the birchtree log are the evidence.
