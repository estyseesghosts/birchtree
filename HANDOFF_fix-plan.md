# Handoff: complete `docs/fix-plan.md` in entirety

## Context (read first, in this order)

1. `AGENTS.md` — architecture rules (domain ← application ← adapters; one
   txn commits state+indexes+jobs; network after commit in idempotent jobs;
   no success-shaped stubs; subagent worktree-partition hard rule) and the
   state-maintenance discipline (dated log per session, append-only).
2. `birchtree.md` §7 (phase gates) + `docs/fix-plan.md` — THE work list
   (P0 verified → P1 decided-build → P2 verify-then-fix → P3 decided).
   If this file and `fix-plan.md` disagree, `fix-plan.md` wins for scope.
3. `docs/compatibility-profile.md` — deviation register (44 IDs; an
   unregistered difference is a bug). `docs/endpoint-manifest.md` (~268
   rows). `docs/configuration.md` + `docs/config-schema.json`.
4. Reference: Mastodon v4.7.3 pinned (`8ff11c2b`), read-only
   (`~/fedifedifedi/mastodon`, `pinetree` symlink). **Cite `file:line`
   both sides for every behavior claim, always against the PIN —
   never `main`/HEAD** (two prior auditors read a drifted checkout and
   their rows need re-verification; see P2).
5. Starting state: `master` at the leftovers-round-4 commit (D12/D39/D42
   remainders closed; schema v12; RFC-locked push KDF). Run
   `git status --short` first, respect uncommitted work, never touch it.
6. Evidence discipline: wire/code evidence before behavior changes;
   executed tests only; `docs/logs/` entry + dated
   `state/logs/YYYY-MM-DD-<topic>.md` per session (append-only).

## Already decided (do not re-ask)

- P0 privacy bugs (F1 reblog-default, F2 block/mute/domain gates, F3
  unreblog gate): fix unconditionally, severity order.
- P1: **build** suggestions, announcements (ranking/authoring surfaces),
  donation → **honest absence** (remove 204 stub, 404 + row).
- P3: federation tail → own phase later (D32 stands); instance
  sub-resources → build; directory → with suggestions; userinfo → build;
  metrics moderator gate → widen now; admin invite/IP stores → build;
  space_usage keys → keep + register; D35 fine-grained perms → roadmap;
  D22 default-show + all nine divergences → accepted intentional.
- Open product questions (ask the operator when reached): password-reset
  redemption HTTP flow shape; suggestion-ranking source if none emerges.

## Work sequence (in order; later steps build on earlier)

0. **Verify-then-queue.** Re-verify every P2 row against the PIN before
   scheduling it. Drop rows that never differed (record the check, don't
   just delete: note the pin cite that clears them). Promote confirmed
   rows into the fix order below. Triage the 30-row federation MISSING
   list against D32 (refuse-position stands; register individual rows
   where client-visible).
1. **P0 privacy** (F1→F2→F3): each with contract tests proving the
   upstream shape (`reblog_service.rb:23-26`, `status_policy.rb:1-30`,
   `reblogs_controller.rb:14-33` at the pin). F2 touches
   `src/application/policy.rs` + all four action gates — one worker, no
   split.
2. **P1 builds + donation removal.** Suggestions engine, announcements
   authoring + reads + reactions, userinfo handler, instance
   sub-resources, directory (with suggestions or honest absence),
   donation 204→404+row, metrics moderator widening, admin invite/IP
   stores. Partition by file set per AGENTS.md (suggestion engine and
   admin stores are the two big ones; keep them on separate workers).
3. **P2 confirmed rows**, severity order: un-fanout overlays → paging
   (quotes/follow-requests/endorsements/collections) → serializers
   (credential/preferences/instance DTO) → dropped params (search,
   account-search, relationships, markers, scheduled) → media/push
   hygiene → timelines params → federation WRONG rows (paging shapes,
   retry budget, webfinger 410, audience match, dedup bound, caps).
4. **Register + manifest pass.** Every remaining divergence gets a row;
   stale rows corrected (D30 paging claim, webfinger/nodeinfo planned
   rows). An unregistered difference is a bug — no exceptions.
5. **Gate + commit.** `cargo check --all-targets` warning-free +
   `cargo test --release` green + HTTP smoke for behavior changes.
   Commit when green; never commit red.

## Standing rules for every subagent you spawn

- Disjoint file sets; shared files (`docs/*`, router/ports, schema,
  migrations, shared tests) parent-merges — subagents propose snippets.
- Uniquely-named test files (`tests/fix4_<area>.rs`), no commits/stash/
  reset, leave uncommitted.
- No PII/secrets in logs; federation input untrusted (size/timeout/
  redirect/SSRF caps); mail/push/crawl after commit in idempotent jobs.
- Each gap closed with test evidence or explicitly remaining with a
  named blocking dependency.

## Non-goals

- Federation-tail phase (plan it, don't build it here). Live interop
  proofs (no live services in env — record, don't chase). D35
  fine-grained perms (roadmap). Runtime config reload (restart model
  stands).
