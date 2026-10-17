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
