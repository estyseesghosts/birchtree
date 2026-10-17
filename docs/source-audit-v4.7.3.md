# docs/source-audit-v4.7.3.md

Status: Draft

Authority: Mastodon v4.7.3 (`~/fedifedifedi/mastodon`, commit `8ff11c2bb7b9f0714e947b10fa3629c6542c7121`), released 2026-10-01. API v7 contract incl. quote additions.

## Files reviewed (per `birchtree.md` §10)

- `config/routes/api.rb` — route inventory basis for `docs/endpoint-manifest.md`
- `app/controllers/api/v1/statuses_controller.rb` — thin adapter: scopes, reply/quote resolution, authorization, services, serialization
- `app/services/post_status_service.rb` — defaults/visibility, idempotency, media, mentions/hashtags/links, distribution/notification/federation scheduling
- `app/services/update_status_service.rb` — edit history, attachments/polls/content, follow-up work
- `app/models/status.rb` + `app/models/concerns/status/visibility.rb` — visibility incl. internal `limited` state
- `app/policies/status_policy.rb` — operation-specific decisions (show/quote/reblog/favourite/edit/delete)
- `app/serializers/rest/status_serializer.rb` — viewer-aware output (favourited/reblogged/muted/bookmarked/pinned, quote data, source mode)
- `app/serializers/rest/account_serializer.rb` — acct/profile/media URLs/counts/moved accounts/emoji/conditional unavailable state
- `app/models/quote.rb` — quote state machine: pending/accepted/rejected/revoked/deleted
- `app/lib/activitypub/activity/quote_request.rb` — remote QuoteRequest → Accept/Reject exchange
- `app/models/account.rb`, `app/models/follow.rb`, `app/models/notification.rb` — social graph records
- `db/schema.rb` — effective access pattern (not ported literally)
- `lib/mastodon/snowflake.rb` — numeric ID with millisecond timestamp component
- `app/lib/feed_manager.rb` — Redis sorted-set home/list feeds (different implementation here; see `architecture.md`)
- `app/controllers/api/v1/streaming_controller.rb` — streaming host discovery/redirect
- `app/lib/activitypub/activity.rb` — ingress activity dispatch
- `app/lib/activitypub/parser/status_parser.rb` — JSON-LD/ActivityStreams normalization
- `app/workers/activitypub/delivery_worker.rb` — signed async inbox delivery, retry, failure tracking
- `config/initializers/doorkeeper.rb` — OAuth configuration source of truth
- `app/controllers/api/v1/media_controller.rb`, `app/controllers/api/v2/media_controller.rb` — media v1/v2

## Key behaviors to reproduce (condensed from `birchtree.md` §2)

- Visibility: public/unlisted/private/direct plus internal `limited` (presented as `private`; never user-creatable or a fifth API value).
- Five visibility predicates: can-fetch, distributable-surfaces, AP-inbox delivery, interaction (quote/reblog/favourite/reply/bookmark/report), withholding (blocks/domain blocks/mutes/unavailability/filters).
- Quotes are a first-class state machine, not `reblog_of_id`: URI/approval URI, `quotes_count`, policy public/followers/nobody, `RE:` textual fallback.
- IDs: numeric bigint internally, decimal strings on the wire, numeric ordering/cursor semantics.
- `Account` (AP identity) vs `LocalUser`/credentials (login) are distinct records.
- Follows store show_reblogs/notify/languages/URI; locked accounts need pending follow requests + Accept/Reject.
- Ingress: authenticate signature/digest, parse+validate untrusted JSON-LD, dedup activity IDs, apply one application command, persist follow-up jobs, ack. Egress: commit jobs before delivery, sign, retry per destination, detect permanent failures, isolate failing hosts.
- No outbound HTTP inside a local status/follow transaction.
- OAuth: authorization-code + client-credentials grants, PKCE S256, exact redirect matching, state, one-time codes, short-lived codes, token revocation; no resource-owner password grant.
- Error envelope: `{"error": "..."}` with real status; Mastodon-shaped, not pinetree/Misskey.

## Differences between public API docs and implementation (to record per endpoint in Phase 0/1)

None recorded yet — pending endpoint-by-endpoint review against `config/routes/api.rb` and the controllers above.
