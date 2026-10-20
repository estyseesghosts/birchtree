# docs/TODOs.md

Status: Draft

Remaining work after the 2026-10-20 fix round (P0 privacy, P1 builds, P2
confirmed rows — all green, see `docs/fix-plan.md` and
`docs/logs/2026-10-20-fix-plan-verification.md`). Each item is honest
absence or a recorded gap — never a success-shaped stub. Items marked
**operator** need an operator decision before build; items marked
**phase** belong to a named future phase.

Conventions: `D##` = row in `docs/compatibility-profile.md`;
"manifest" = `docs/endpoint-manifest.md`.

## Needs an operator decision

- **Password-reset redemption flow.** The "forgot password" mail sends a
  real link, but the link target (`/auth/password/edit`-equivalent) has no
  page and no set-password port. Shape of the redemption HTTP flow is
  undecided.
- **Suggestion ranking source.** Suggestions rank honest local signals only
  (friends-of-friends, then most-followed fill). Confirm this is enough, or
  name a richer source (staff picks, similar-profiles, FASP, remote
  signals). Related: the profile directory is always on (upstream has a
  kill switch we did not build).
- **Site text settings.** Terms of service, privacy policy and extended
  description answer 404 — there is no authoring surface for the texts.
  Decide the settings/authoring shape (then the instance sub-resources serve
  them instead of 404ing).

## Media / profile follow-ups

- **Avatar/header upload in `update_credentials`.** `bot`, profile fields
  and attribution domains persist; `avatar`/`header` (+ descriptions) need
  the media pipeline (Phase 7.1 transcode + attachment ownership checks).
  Manifest notes it as a named follow-up.
- **Rule translations.** Server rules render `translations: {}` — no
  translation store exists.
- **Locale catalog for languages.** `GET /api/v1/instance/languages`
  renders `name` = code (e.g. `"fr"` instead of `"Français"`); upstream
  serves localized names from its ISO-639 catalog.

## Notifications / streaming follow-ups

- **Unproduced notification types (D51).** Ten types parse but are never
  emitted (poll updates, post updates, severed relationships, moderation
  warnings, annual reports, admin sign-ups/reports, quote updates,
  added-to-collection, collection updates). One register row per producer as
  the underlying features land.
- **Streaming announcement events.** Upstream emits `announcement`,
  `announcement.reaction` and `announcement.delete` on streams; we emit
  none (reads/dismissals/reactions are REST-only so far).
- **Store push-down for notification paging (deferred, not a bug).** The
  `account_id` fill loop is correct and cursor-stable; pushing the actor
  filter into the store page is a bounded perf optimization. Revisit only
  with measured scan pressure.
- **`Unsubscribe-URL` on pre-token push rows.** Subscriptions created
  before the fix round carry no token/URL and send no header (never
  fabricated). New subscriptions are complete.

## Federation tail (own phase — D32 refuse-position stands)

- Account Move (migrated follows need an individual register row).
- Remote emoji ingest (`emojis` arrays omit remote emoji).
- Relays / inbox forwarding (delivery reachability claims).
- Follower sync and remote counters.
- Note / featured collections.
- Quote re-verify workers, remote counts, note collections.
- Poll `collection_update` notifications.
- Live interop proofs (no live services in this environment — recorded,
  not chased).

## Collections surface

- `hide_collections` round-trips on the account row but there are no
  collections to hide (D52).
- Unbuilt endpoints (in manifest "not yet rowed" list): account `lists`
  sub-resource, `identity_proofs`, `email_subscriptions`, `in_collections`,
  top-level `collections` CRUD + items/revoke, `v1_alpha` namespace,
  `domain_blocks/preview`, `GET/PATCH /api/v1/profile`, instance
  `peers/search`, `GET /api/v2/notifications` family.
- `reblogged_by` lacks upstream's `distributable_visibility` scope (D52).

## Permissions / admin follow-ups

- **D35 roadmap:** fine-grained per-action permission flags on the upstream
  `Admin::BasePolicy` model (shipped: two-role split, moderator covers
  accounts+reports only).
- **Role serializer:** role ids are names and permissions are empty (no
  numeric ids / bitmask census).

## Deliberately out of profile (not TODO, listed so nobody re-files them)

- Translation provider (`POST /api/v1/statuses/:id/translate`).
- `gifv` output (WebM/VP9 by decision D1–D4).
- Runtime config reload (restart model stands).
- Web UI beyond login/consent/first-party bundle; admin dashboard HTML;
  announcements HTML authoring (API surface built instead).
- FASP-sourced suggestions (`Mastodon-Async-Refresh` on v2).
- PostgreSQL/Redis/external broker, multi-node/cluster.

## Performance follow-ups (deferred, not correctness)

- `PairState` extension (carry mute/domain facts in the pair row instead of
  extra store reads).
- Notification store push-down (see above).
