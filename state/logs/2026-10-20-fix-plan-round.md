# 2026-10-20 — fix-plan execution (P0/P1/P2-triage)

## What happened
- Launched 5 partitioned subagents (background): P0 privacy (F1–F3), P1
  suggestions+directory+userinfo, P1 announcements, P1 instance/donation/
  metrics/admin-stores, P2 verify-then-queue triage (read-only).
- Triage returned first: full verdict table vs pin `8ff11c2b` (checkout HEAD
  `7f90e0fd` correctly ignored). ~2/3 of P2 confirmed+queued (severity
  order, groups a–g); 20 rows refuted with record (incl. 2 auditor
  hallucinations: `state`/`spoiler_field`); 3 rows downgraded to test-only.
- P0 worker completed: F1/F2/F3 fixed in `src/application/policy.rs`,
  `src/application/interactions.rs`, `src/application/quotes.rs`;
  `tests/fix4_p0_privacy.rs` 13 pass, policy unit 7 pass.
- Parent (this session): merged triage into docs — `docs/fix-plan.md`
  status block (queue + refuted + seams), `docs/compatibility-profile.md`
  D30 reopened / D32 ingest-scope / new D45–D47 (F2 strictness deltas),
  `docs/endpoint-manifest.md` webfinger+nodeinfo → implemented, quotes-row
  deviation pointer, "not yet rowed" prune.

## Verification (executed, real output)
- `cargo test --test fix4_p0_privacy`: 13 passed, 0 failed (worker report).
- `cargo test --lib application::policy`: 7 passed (worker report).
- `git status --short`: each worker touched only its files; shared
  `src/application/mod.rs` got one line (`pub mod suggestions` — parent adds
  `announcements` at merge).
- Grep-verified before editing: webfinger/nodeinfo routes served in
  `src/activitypub/http.rs:42-44`; blocks/mutes/favourites/bookmarks/
  familiar_followers/featured_tags/followed_tags/tags/policy rows present in
  manifest (prune was safe); compat table pipe counts = 6/row after D45–47.

## Open questions / seams
- Password-reset redemption HTTP flow shape (operator question, unchanged).
- Suggestion-ranking source if none emerges (worker chose honest local
  signal; operator confirms on return).
- `PairState` extension snippet deferred — perf cleanup, not correctness.
- F3/P2 overlay wiring in status DTO layer open until P2 workers land.
- Remaining workers (3× P1 builds) still in flight; P2 fix workers launch
  after. Gate (`cargo check --all-targets` + `cargo test --release` + HTTP
  smoke) runs at parent merge when worktree is quiet.

## Merge session (parent, same day)

All four build workers returned green; parent merged their parent-owned
pieces into the shared worktree (no worker files touched except completed
workers' tests/endpoint shapes where the merge required it):

- **Suggestions**: `SuggestionStore` + redb `SUGGESTION_MUTES` (schema v13),
  member `DELETE /api/v1/suggestions/:id` with persisted dismissal, v2
  destroy route removed (405 on the GET path vs upstream 404 — recorded),
  `/api/v1/directory` + `/oauth/userinfo` wired. `tests/fix4_suggestions.rs`
  updated to member shape + persistence assertion: 7 pass.
- **Announcements**: `AnnouncementStore` + 5 redb tables (schema v14),
  endpoints swapped from `MemoryAnnouncements` to durable store, 7 admin
  routes mounted (`AdminOnly`). 6 integration + 7 lib tests pass.
  Scheduled publish stays manual (no publish job — recorded).
- **Instance**: 10 sub-resource routes + `peers_api_enabled` (default on),
  `activity_api_enabled` (default on), `domain_blocks_public` (default off)
  config flags with `ServerInfo` mirrors; donation 204→404; metrics
  moderator widening (already in worker files). 9 tests pass.
- **Admin identity stores**: `LocalUser.locale`/`invite_request`,
  `SignInEvent.ip`, `INVITE_USES` (schema v15, same-txn write),
  `invite_code_for_account`/`accounts_for_inviter`/`login_ips_for`,
  serializer + `invited_by`/`ip` filters (garbage `invited_by` → `[]`,
  verified against `account_filter.rb:117-119` at the pin).
- **Incidental fix**: HTTP registration ignored invite codes
  (`register_account_with_invite` was dead code); endpoint now calls it,
  making the D12 "usable invite opens closed registrations" claim real.
  `ServerInfo.invites_enabled` mirror added.
- Evidence: `cargo check --all-targets` clean; targeted suites green
  (fix4_suggestions 7, fix4_announcements 6, fix4_instance_admin 9+1 new,
  round3_admin_stats 6, phase4_1_home_cap 4).
- Open for P2 workers: `reblogs_map:false` overlay wiring, PairState
  perf follow-up, publish-scheduled job, `site_terms`/`extended_description`/
  `privacy_policy` text settings, `UserRole` numeric ids, rule translations,
  locale catalog for languages.

## P2 launch plan (queued; launch when smoke-all is green)

Three workers, disjoint file sets (router/ports/schema/migrations/dto-shared
parts + `docs/*` parent-owned; subagents propose snippets):

- **P2a statuses/paging** (`tests/fix4_p2_status.rs`): owns
  `src/application/statuses.rs`, `quotes.rs`, `relationships.rs`
  (follow-request cursor, endorsements, lists `limit=0`/unknown-id checks),
  `src/api/mastodon/endpoints/statuses.rs`,
  `relations_extra.rs`/`relationships.rs` (hide-gate #12e, edge lists),
  `filters.rs` (PUT/PATCH + deprecation headers).
  Queue: #2 quotes cursor→quote id, #3b revoke renders quoting status (with
  parent: endpoint presenter), #4 context self-reply promotion, #6
  favourited/reblogged_by exclusions, #7 quotes mute/domain/silence filter
  (overlaps F2 — reuse `viewer_side_facts`), #13a/#13b follow-requests,
  #14a–e lists/endorsements/filters, #3b overlay part, F3 `reblogs_map:false`
  overlay wiring (parent-owned DTO layer — worker proposes, parent applies).
- **P2b accounts/notify/params** (`tests/fix4_p2_accounts.rs`): owns
  `src/application/accounts.rs`, `search.rs`, `markers.rs`,
  `scheduled_statuses.rs`, `notifications.rs`, `polls.rs`,
  `conversations.rs`, `src/api/mastodon/endpoints/accounts.rs`,
  `search.rs`, `markers.rs`, `scheduled_statuses.rs`, `notifications.rs`,
  `polls.rs`, `conversations.rs`, **and `src/api/mastodon/dto.rs`**
  (credential source/role, preferences, update_credentials media-less
  fields). Queue: #11a/#11c serializers, #11d update_credentials
  (bot/fields/attribution now; avatar/header need media pipeline — split),
  #12a–c search/lookup/`with_suspended`, #21h notification params, #21i new
  types (parse/serialize; producers rowed as gaps), #9a empty-choices,
  #10a markers `lock_version` wire-through, #10b scheduled empty no-op,
  #21j conversation-mute 422, #21k allowed_mentions, #8a
  `without_suspended`.
- **P2c push/media/federation** (`tests/fix4_p2_fedpush.rs`): owns
  `src/application/push.rs`, `media.rs`, `timelines.rs`, `trends.rs`,
  `src/application/streaming.rs` + `src/streaming/*` (hashtag normalization),
  `src/activitypub/**`, `src/federation/**`,
  `src/api/mastodon/endpoints/push.rs`, `media.rs`, `timelines.rs`,
  `timelines_link.rs`, `reports.rs`, `instance.rs` (DTO values only).
  Proposes `dto.rs` instance-DTO snippets (parent applies — P2b owns dto).
  Queue: #17a instance DTO config values, #16a/#16b/#16c push hygiene,
  #17c nodeinfo keys (+#17b rebrand register), #18 reports params, #19
  outbox/followers pages-or-amend-D30, #20a–f federation WRONG rows (with
  #20d audience exact-match pulled forward, sev 2), #21a tag any/all/none,
  #21d locked-down gates.
- Gate per worker: contract test proving upstream shape (`git show
  8ff11c2b:` cites, never the drifted worktree) + register row where a
  divergence remains; `cargo test --test fix4_p2_*` + one
  `cargo check --all-targets` (lock contention: wait/retry).
