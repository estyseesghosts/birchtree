# docs/fix-plan.md

Status: Draft

Fix plan for the source-level audit (2026-10-20, four read-only workers).
P0 items below were re-verified by the parent against the pinned tag
(`8ff11c2b`) and our source — they are committed work. P1 stubs are
verified present; build-vs-remove is an operator call each. P2/P3 need a
pin-verification pass first (two auditors read upstream `7f90e0fd`, not the
pin) — they are triage queue, not work items yet.

Sequencing: lands after the in-flight D12/D39/D42 workers (adjacent files),
one fix round, severity order. Gate per item: contract test + register row
where a divergence remains.

## Status 2026-10-20 (fix round, parent log)

- **Step 0 triage DONE (read-only worker).** Every P2 bullet re-verified
  against the pin (`8ff11c2b`; the checkout HEAD `7f90e0fd` was never read).
  CONFIRMED queue (severity order within group): paging — quotes cursor→quote
  id (#2), follow-request cursor namespace + `since_id` (#13a), endorsements
  `unlimited?` + `without_suspended` (#14c); serializers — credential source
  real fields/discoverable/hide_collections/indexable/attribution + real role
  (#11a), preferences language source + `expand:media` values (#11c),
  instance DTO config values + real `source_url` (#17a), quotes-revoke renders
  quoting status (#3b), VAPID exp 24h (#16b), nodeinfo `metadata` key (#17c);
  params — `update_credentials` bot/fields/attribution then avatar/header
  (#11d), followers/following hide-gate (#12e), notification types (#21i),
  search/lookup/`with_suspended` (#12a–c), notification params (#21h), tag
  any/all/none (#21a), reports category/forward/rules/collections (#18),
  `allowed_mentions` (#21k), poll empty-choices (#9a), scheduled empty no-op
  (#10b), authorize notification (#13b), conversation-mute 422 (#21j),
  markers `lock_version` wire-through (#10a), filter PUT/PATCH + deprecation
  headers (#14d/e); media/push — cleanup all-4xx-except-408/429 (#16a),
  `Unsubscribe-URL` (#16c); timelines — locked-down auth gates (#21d),
  context self-reply promotion (#4, depth REFUTED); federation — audience
  exact-match (#20d, sev 2), favourited/reblogged exclusion (#6) + quotes
  mute/domain filter (#7) with F2, notification `without_suspended` (#8a),
  outbox/followers paging or amend D30 (#19), bounded dedup + D33 (#20b),
  webfinger 410 (#20c), retry budget re-budget-or-register (#20a), sensitized
  forcing (#20e), language/caps/timestamps (#20f), poll additive re-vote
  (#9b). REFUTED with record (drop, don't recode): unfav/unbookmark
  overlays (sync commit ⇒ equal values), quotes revoke authz, context depth,
  multi-fetch order, notification 404 body, `allow_nonfunctional`,
  `state`/`spoiler_field` (auditor hallucination), self-follow 403,
  media preview/guard, push invalid-row destroy, home `local`, link 404-gate,
  list replies-policy, trends locale; list `limit=0`/unknown-ids + endorsements
  paging downgraded to test-only (manifest claims parity).
  Federation MISSING vs D32: refuse stands for ingest; individual rows needed
  for Move, remote emoji, relays, follower-sync counts, note/featured
  collections. Stale flags fixed 2026-10-20: D30 reopened (count-only, no
  pages), webfinger/nodeinfo rows → implemented, D32 scoped to ingest,
  quotes/manifest pointers added, "not yet rowed" list pruned.
- **P0 DONE 2026-10-20.** F1 (default-privacy reblog incl. hidden-target
  wins over explicit param), F2 (viewer-side + mute + domain gates; three
  beyond-upstream strictness deltas registered as D45/D46/D47), F3
  (unreblog no gate on destroy). Evidence: `tests/fix4_p0_privacy.rs`
  (13 pass) + `application::policy` unit (7 pass). Seams for parent/P2:
  `PairState` extension snippet deferred (impl works via `viewer_side_facts`);
  F3/P2 overlay wiring (`reblogs_map:false` + decremented count,
  `favourites_map:false`, `bookmarks_map:false`) belongs in the status
  DTO/presenter layer — P2 queue covers the reblog overlay; unfav/unbookmark
  overlays REFUTED above.
- **P1/P3 build workers DONE 2026-10-20** (suggestions+directory+userinfo,
  announcements, instance/donation/metrics/admin-stores, all green on
  return). Parent merges (same session): durable suggestion dismissals
  (schema v13, member DELETE, v2 destroy removed); durable announcements
  (schema v14, admin routes mounted); instance sub-resources (10 routes +
  3 config flags); donation 204→404; metrics moderator widening; admin
  identity stores (locale/invite-request/INVITE_USES schema v15/login IPs,
  serializer + `invited_by`/`ip` filters). Incidental parent fix: HTTP
  registration now consumes invites (`register_account_with_invite` was
  dead code — D12 claim made real).
- **P2b/P2c workers DONE 2026-10-20**; parent merged their follow-ups:
  stored profile fields (bot/discoverable/indexable/hide_collections/fields/
  attribution), reading-preference extension setters, notification 17-type
  parse + filterable table, reports persistence, audience exact-match,
  dedup TTL policy + amortized prune (no scheduler), nodeinfo metadata,
  feed-access gates (4 config flags), token-per-API-sub + `Unsubscribe-URL`
  + token-first API destroy, `hide_collections` noted (no store yet).
  Deferred with record: store push-down for notification paging (fill loop
  is correct), avatar/header pipeline, `site_terms`/extended/privacy text
  settings, scheduled publish job, rule translations, locale catalog.
- **P2a worker DONE 2026-10-20** (context promotion, quotes silence gate,
  favourited/reblogged exclusions, follower hide-gate, endorsements
  unlimited, v1 deprecation headers; revoke/no-op overlays verified
  wire-equal). Parent merges: phase3 context order fix, filter PUT/PATCH
  routes, lists `limit=0`, authorize→authorizer notification (corrected from
  propose-add to replace: the follower-directed job was wrong-direction),
  follow-request `since_id` (namespace registered D50, not re-keyed),
  quotes cursor rekey + schema v16 migration. P2 fix workers launch on the
  CONFIRMED queue above when the worktree is quiet.

## P0 — privacy bugs (verified, fix unconditionally)

### F1. Reblog defaults to Public instead of the user's default privacy
- Upstream (`app/services/reblog_service.rb:23-26` at `8ff11c2b`):
  `options[:visibility] || account.user&.setting_default_privacy`
  (hidden target → target visibility).
- Ours (`src/application/interactions.rs`, `reblog`): `None =>
  Visibility::Public`. A user whose default is private/followers-only boosts
  publicly. Privacy-widening.
- Fix: read the viewer's `default_privacy` (already stored on
  `LocalUser`) when no visibility is passed; keep hidden-target rule.
- Test: reblog without visibility by a private-default user → private
  reblog; explicit visibility still honored.

### F2. Viewer-side block/mute/domain ignored on fetch and action gates
- Upstream (`app/policies/status_policy.rb:1-30` at `8ff11c2b`): `show?`
  denies when the author blocks the viewer **or the author's domain**
  (`!author_blocking? && !author_blocking_domain?`); `reblog?`/`favourite?`
  additionally deny when the **viewer blocks the author**
  (`!blocking_author?`); `quote?` same.
- Ours (`src/application/policy.rs:27-55`): `can_fetch` checks only
  author-blocks-viewer; no domain check anywhere on show; `can_reblog` /
  `can_favourite` / `can_bookmark` / quote gate lack the viewer-blocks-author
  check. Effect: blocked/muted/domain-blocked authors' posts stay fetchable
  and actionable (favourite/reblog/quote) where upstream refuses.
- Fix: add viewer-side + domain-block predicates to `can_fetch`
  (show-equivalent) and the viewer-blocks-author check to
  favourite/reblog/bookmark/quote gates. Keep D22 (home-feed mention
  display, accepted) untouched — this is about fetch/action, not home.
- Test per gate: blocked-author post → fetch 404-equivalent, favourite /
  reblog / quote refused; domain-blocked author public post → refused.

### F3. Unreblog gated on `can_reblog` (upstream: no gate on destroy)
- Upstream (`app/controllers/api/v1/statuses/reblogs_controller.rb:14-33`):
  destroy has no reblog-gate; no-op returns the ORIGINAL with a
  `reblogs_map:false` + decremented-count overlay, 200 even when the target
  is no longer rebloggable.
- Ours (`src/application/interactions.rs`, `unreblog`): requires
  `can_reblog` → 403 where upstream 200s.
- Fix: drop the gate on the destroy path (keep it on create); return the
  original on no-op. Count overlays (here + unfavourite/unbookmark, P2)
  follow the same controller shapes — verify pin, then implement together.

## P1 — success-shaped stubs (verified present, build or remove)

### S1. Suggestions (`src/api/mastodon/endpoints/suggestions.rs`)
Always `[]`; deletes answer `204` (upstream: real suggestion objects,
destroy `200 {}`). **Decided (operator, 2026-10-20): build** — a real
ranking source; profile directory folds into this work if one emerges.

### S2. Announcements (`src/api/mastodon/endpoints/announcements.rs`)
Always `[]`; dismiss/react are `204` no-ops (upstream: real list, dismiss
`200 {}`, reactions with a feature-flag `400`). Announcements are
operator-authored — there is no authoring surface at all. **Decided
(operator, 2026-10-20): build** — admin authoring path + reads + reactions.

Rule for both: no third option. A stub with a register note is still a stub.

## P2 — likely bugs (verify pin, then fix in the same round)

- Unfavourite/unbookmark no-op payloads (missing `favourites_map:false` /
  count-1 and `bookmarks_map:false` overlays).
- Quotes index paged by quoting-status id instead of quote id (drift/dupes).
- Quotes revoke authz + rendered object shape.
- Context descendant ordering (no self-reply promotion) and depth
  truncation boundaries.
- Multi-fetch order (comment claims descending, code sorts ascending).
- `favourited_by`/`reblogged_by` missing blocked/muted exclusion.
- Quotes index missing mute/domain/silence filter.
- Notification reads missing `without_suspended` scope (+ 404 body shape).
- Poll vote empty-choices (400 vs silent no-op) and multiple-choice
  incremental re-vote semantics.
- Markers `lock_version` ignored (no 409); scheduled-update empty no-op
  422s; `verify_credentials allow_nonfunctional` absent.
- Credential serializer shape (`role`, `state`, `source` incl.
  `spoiler_field`, real `discoverable`/`fields`); preferences keys;
  update_credentials drops avatar/header/bot/fields/attribution/spoiler.
- Account search scope/params (`resolve`/`following`/auth); lookup resolves
  local-only; relationships `with_suspended` ignored; self-follow not 403;
  locked/hidden followers/following lists visible.
- Follow-request paging (`since_id`, id space) + missing authorize
  notification.
- List `limit=0` (empty vs all), unknown member ids skipped vs 404;
  endorsements paging; filter v1 PATCH / v2 PUT missing; v1 deprecation
  headers.
- Media: `preview_url`/`meta` nullability during processing; attached-media
  still readable via `GET /media/:id` (needs `status_id` guard check).
- Push: 4xx cleanup scope (only 404/410 vs all-but-408/429), VAPID `exp`
  12h vs 24h, missing `Unsubscribe-URL` header, no invalid-row destroy.
- Instance DTO hardcoded fields (`approval_required`, `invites_enabled`,
  mime types, vapid key, icons, thumbnail variants, `source_url`
  placeholder); NodeInfo software name (`birchtree` vs `mastodon` —
  confirm rebrand intent); nodeinfo metadata empty; featured-tag
  name/url forms; custom emoji `category` null-vs-absent + `featured`.
- Reports hardcode `category:"other"`, drop forwards/collections.
- Outbox/followers collection paging shapes (contradict the D30
  "counts and pages match" claim — correct the code or the claim).
- Delivery retry budget (5 tries vs upstream's days-long tail); dedup
  store unbounded (needs TTL/bound); WebFinger suspended→410;
  visibility audience exact-match (`ends_with("/followers")` too loose);
  `sensitive` forcing from sensitized remotes; language normalization;
  poll/attachment caps; timestamp validation.
- Timelines: home exclusion params ignored; tag `any/all/none`; link
  404-gate; public/tag auth gates on locked-down instances; list
  replies-policy; hashtag subscription normalization; trends locale +
  per-user filtering; `account_id`/`include_filtered`/`supported_types`
  notification params; 10 missing notification types (poll, update,
  admin.*, moderation_warning, collections…); conversation-mute 422-vs-404;
  status `allowed_mentions`.

## P3 — decided (operator, 2026-10-20)

- Federation long tail (Move/Add/Remove, relays, forwarding, emoji
  ingest, remote counts, note collections, follower sync): **build the
  full tail** — scoped as its own phase post-fix-round, not smuggled into
  it. D32 refuse-position stands until each piece lands.
- Instance sub-resources (peers/rules/languages/activity…): **build**
  (mostly config/existing-data reads) in the fix round.
- Profile directory: **build alongside suggestions** if a ranking source
  emerges; else honest absence.
- OAuth userinfo: **build** (small: token-scope claims handler).
- Donation campaigns: **honest absence** — remove the 204 stub, 404 + row.
- Metrics moderator gate: **widen now** (moderators read analytics;
  preempts part of D35).
- Admin invited_by/IP/locale/invite fields: **build the stores**
  (privacy-sensitive; careful review).
- `space_usage` keys: **keep ours + register** the rename as accepted.

## P3 — needs a decision or is out of scope (pre-decision text kept below
for provenance; each item above is now decided)

- Federation long tail (Move/Add/Remove, relays, forwarding, emoji
  ingest, remote counts, note collections, quote re-verify workers,
  follower sync): confirm the D32 refuse-position covers each, register
  any that need individual rows.
- `GET /api/v1/directory`, instance sub-resources (peers/rules/languages/
  activity…), `oauth/userinfo`: build, honestly absence, or out-of-profile.
- Donation campaigns (unconditional 204 — outbound fetch unbuilt).
- Admin role gate on measures/dimensions/retention (moderators 403;
  partially D35-roadmap).
- Admin `invited_by` filter, IP/locale/invite serializer fields (need
  stores we don't have — register or build with D12 email index now
  landing).
- `space_usage` key rename (`media,database` vs upstream
  `postgresql,redis,media,search`) + missing `search` entry.
- Streaming announcements/`filters_changed`/health-metrics surface.
- Live proofs (unchanged): lab interop, live push, live SMTP.

## Counts (audit, pre-verification)

- Accounts/auth: 33 wrong (3 minor) + 1 missing + 6 registered.
- Statuses/timelines/notifications: 25 actionable wrong + 27 missing + 4
  registered (5 auditor rows withdrawn on re-read).
- Media/push/admin: 20 wrong + 14 missing + 6 registered.
- Federation: 14 wrong + 30 missing + 9 registered.
