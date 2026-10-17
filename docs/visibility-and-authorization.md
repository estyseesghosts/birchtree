# docs/visibility-and-authorization.md

Status: Draft

Visibility and authorization policy contract (per `birchtree.md` §4.3).

## Visibility levels (public API)

| Level | Fetchable | Distributable | Notes |
|---|---|---|---|
| `public` | yes | yes | public timeline eligible |
| `unlisted` | yes | no | excluded from public timeline distribution |
| `private` | follower-targeted | follower inboxes | viewer relationship + mention rules apply |
| `direct` | mention-targeted | mention inboxes only | must not leak through public/list/search/stream |
| `limited` | n/a | n/a | internal moderation state; projected as `private`; never user-creatable |

## Named policy predicates

- `can_fetch(viewer, status)` — can this viewer fetch this status?
- `distributable_surfaces(status)` — public/local/home/list/hashtag/search eligibility
- `can_deliver_to_inbox(status, inbox)` — ActivityPub outbound delivery eligibility
- `can_interact(viewer, status, op)` — op ∈ {quote, reblog, favourite, reply, bookmark, report}
- `withheld(viewer, status, reason)` — blocks, domain blocks, mutes, unavailability, filters

Blocks and domain blocks apply consistently to read/interaction/distribution. Mutes/filters suppress feed/notification display only; never used as the sole authorization check. "Hidden from this timeline" is never the only authorization check.

Quote policy: `public`/`followers`/`nobody`; private/direct statuses force `nobody`. Direct/private mention quotes get additional mention authorization.

## Scopes (OAuth, enforced centrally in Phase 2+)

`read`, `write`, `follow`, `push`, `profile` plus endpoint-level `read:statuses`, `write:statuses`, `read:accounts`, `read:notifications`, etc. App-only (client-credentials) tokens are not user tokens; endpoint authorization distinguishes them.

## Implementation status (Phase 3)

The predicates live in `application/policy.rs` and are the single source for
every read surface and interaction gate; `RelationshipStore::pair_state`
supplies the viewer/author relationship facts (block/follow/mute, expiry-aware).

- `can_fetch(viewer, status, pair)` — `public`/`unlisted`: anyone except a
  viewer the author blocks; `private`: author, followers (`pair.followed_by`),
  mentioned (visible or silent); `direct`: author or mentioned only; deleted
  (tombstoned) statuses are never fetchable.
- `can_reblog` — fetchable, visibility ∈ {public, unlisted, private} (never
  direct), private only by its owner; denials answer `403 This action is not
  allowed` while unfetchable statuses answer `404`.
- `can_favourite`/`can_bookmark` = `can_fetch` (block covered by the pair).
- `quote_permission` → `SelfQuote`/`Automatic`/`Denied`: self always allowed;
  `public` allowed; `followers` requires `pair.followed_by`; `nobody`, direct
  visibility, reblogs and unfetchable targets are denied. Denials surface as
  the pinned `404 The post you are trying to quote does not appear to exist.`
  (the source cannot distinguish missing from non-quotable).
- Pins: owner-only, never reblogs, never direct (422 exact messages),
  maximum 5 per account.
- Blocks/domain blocks gate reads (per the table above), follows and
  interactions; mutes suppress nothing at authorization time (display
  suppression is a Phase 4 read-surface concern) — mute state feeds the
  relationship DTO and notification flags only.
- Quote-policy downgrade: local `private`/`direct` statuses store
  `quote_approval_policy = nobody` regardless of the submitted value
  (`downgrade_quote_policy` parity), and the interaction-policy endpoint
  silently keeps the downgraded value (still 200).
