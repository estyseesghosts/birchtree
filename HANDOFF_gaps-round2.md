# Handoff: Birchtree — gap-closing round 2 (minors, federation edges, D15/D20/D21/D24/D36–D40)

## Context (read first)

- Plan: `birchtree.md` §7 Phases 7/7.1/7.2/7.3 exit gates + §8 item 7 (federation suites). Architecture rules: `AGENTS.md`.
- Gap register: `docs/compatibility-profile.md` rows D15, D20, D21, D24, D36–D40 (each states upstream behavior, our deviation, and why). New rows for anything you deliberately change; a difference not in the register is a bug.
- Contract: `docs/endpoint-manifest.md` (224/226 implemented — keep it that way; flip/update any row your work affects).
- Reference: Mastodon v4.7.3 pinned tag (`8ff11c2b`), read-only. Cite `file:line` on both sides for behavior claims.
- Starting state: routes complete; transcode remux amendment committed (`f9c2c7c`); tag-inbound/edit-recompute work **in flight by another implementer (uncommitted)** — see collision rule below.
- Evidence discipline: wire/code evidence before behavior changes; `docs/logs/` entries for executed verification; state log per session.

## Objective

Close the listed gaps, each with tests + register updates. Do not claim a gap closed without executed evidence (a passing test, not a reading of the code).

## Instructions (use subagents)

You MUST use subagents for parallel implementation. Suggested split (run in parallel; each owns its gaps + tests + register rows):

- Subagent A (review minors, all small and parallel-safe):
  1. Orphan sweeper for staged/crashed transcode blobs (today: overwrite-by-retry + best-effort delete only; quarantined-while-`Pending` rows and SIGKILL `/tmp` files orphan permanently).
  2. Dead `TranscodePlan`/`JobKind::TranscodeMedia` arm: either wire it (enqueue under it) or delete it — no dead dispatch in prod.
  3. Operator-expose VP9/WebP quality knobs (config + CLI + validation); remove "until the wiring patch" stale comments.
  4. Search: runtime admin toggle for `search_mode` (today CLI+restart only) or downgrade D6's "administrator switch" wording to deploy-time; document the cold-index DB-scan fallback + historical-unlisted backfill gap in the user contract.
  5. `DELETE /api/web/push_subscriptions/:id` numeric-id path: add `check_csrf` when the session user acts (capability-token path stays exempt, document why).
  6. D23 follow-languages clear path (`languages=[]` semantics vs unfollow/re-follow; pick one, test it, register it).
  7. Byte-vs-char length quirks in the search tokenizer (`MAX_TOKEN_LEN`, `MIN_QUERY_TOKEN_LEN`): one contract line each, or normalize to chars.
  8. Stale comments sweep in touched modules (`TranscodeMedia` wiring notes, `JobKind` parent notes).
- Subagent B (streaming + session policy: D15, D20, D21, D24):
  - D15: match upstream — retract *all* of a blocker's/muter's home+list stream statuses, not 200. Implement batched/bounded so a 10k-status backlog can't stall the connection; if latency fails, escalate with measurements instead of silently keeping the cap. Update the D15 row (recommended → decided/implemented).
  - D20: `notifications_merged` requires the notification-request filter pipeline (today `requests` is always `[]`). Implement the minimal pipeline (filter → merge → event) or record an explicit planned-gap entry — no event without the pipeline behind it.
  - D21: `chosen_languages` doesn't exist as a setting. Add user language settings (Phase 2 settings gap), apply to public + hashtag streams like upstream; state whether REST timelines also filter (verify against source, don't guess).
  - D24: kill (revoke) tokens on account suspension/disablement; open streams for killed tokens must close. Test: suspended account's socket closes, token reuse → 401.
- Subagent C (admin gaps D36, D37, D39):
  - D36: registration approval modes (pending queue, real `approve`/`reject`, `pending` filter matches). Email confirmation is a separate Phase 2 gap — implement modes without inventing email enforcement; document exactly which flows stay email-gated.
  - D37: Rule model (report `rules` populated, not `[]`) + account warnings (warn action with `report_id`; the `none`-without-`report_id` 422 stays).
  - D39: sign-in/login telemetry + feasible metric keys; retention cohorts stay status-authorship-based unless sign-in cohorts are implemented — each key either served or kept on the refused list with its reason. No fabricated values.
- Subagent D (heavy lifts: D38, D40):
  - D38: async account-deletion worker (statuses actually removed, bounded/restartable) + federated `Delete` distribution for the deleted account's objects. This is the top-priority gap — do it first within this subagent.
  - D40: trend-score pipeline (score-ordered, not recency), link-preview crawler through the safe federation client (SSRF/size/time/redirect rules apply), tag follows + history. Locale stays ordering-only unless real locale ranking ships.
- Subagent E (federation edges, from the Phase 6 exit checklist):
  - Per-destination delivery cooldown + failure budget (exit row unchecked: failing hosts currently retry on per-job backoff only).
  - Tombstones for the Delete-beats-Create race (S6 residue: bt-a/b/c `duplicate` residue) — design per `birchtree.md` tombstone guidance, both directions.
  - Response size + content validation for remote documents (exit row unchecked; inbox has the crate's 200 KB via D27, document fetches need their own tested limits).
  - Lab-Mastodon media flakiness: VERIFY-ONLY (proven mastodon-side in `docs/federation-contract.md`). Do not contort emit/fetch to satisfy it; no code without new wire evidence contradicting the run5 verdicts.

Synthesize B+C+D before merging: D36 approval states interact with suspension (D24 token kill) and deletion (D38); D40's crawler shares the safe-client caps E is tightening.

## Collision rule (another implementer is active)

Tag-normalization inbound (`src/activitypub/wire.rs`, `src/application/federation_inbound.rs`) and tag-recompute-on-edit (`src/application/statuses.rs`, `src/persistence/redb/statuses.rs`) are being worked **uncommitted**. Do not edit those paths; rebase/retest against them before claiming D25-adjacent work done. If you must touch a shared file (ports, schema, migrations), coordinate — check `git status` for uncommitted work first and commit only your own files.

## Implementation rules

- No success-shaped stubs: unbuilt stays absent (404); refused metric keys stay refused with reasons; no fabricated trend scores, warnings, or telemetry.
- Architecture rules per AGENTS.md: domain has no HTTP/DB shapes; one txn = state + indexes/counters + jobs; network/ffmpeg/crawl work after commit in idempotent jobs with bounded retry → quarantine.
- **Update `docs/endpoint-manifest.md` (any affected row) and `docs/compatibility-profile.md` (D15/D20/D21/D24/D36–D40 state changes + any new deviation) as you go.** A gap closed without its register row updated does not count as done.
- Append one dated log to `state/logs/YYYY-MM-DD-<topic>.md`; never rewrite past logs.
- Gate: `cargo check --all-targets` warning-free + full `cargo test --release` green (baseline 494/494 on `f9c2c7c`), plus HTTP smoke for the behavior changes (401/403/422 shapes included).

## Deliverables

- Each listed gap either closed (test + register row) or explicitly remaining with its blocking dependency named.
- D38 deletion worker + federated deletes (priority), D36 approval modes, D37 rules/warnings, D39 served-or-refused keys, D40 scores/crawler/tag-follows, D15 full retraction, D20 pipeline-or-gap-entry, D21 settings+filtering, D24 token kill, E cooldown/tombstones/doc-limits, A all eight minors.
- Manifest + register accurate to the code.

## Non-goals

- Do not touch the in-flight tag work, transcode plans/packer, search indexing, or conversations/push/web code except through the listed items. Do not run the §8 item 11 parity suite (separate milestone). Do not decide HEIC/TIFF acceptance (open operator decision) — leave the 422 + exit-claim mismatch for the operator.
