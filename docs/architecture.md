# docs/architecture.md

Status: Draft

Repository layout (one Cargo package, modular monolith; per `birchtree.md` §3):

```
birchtree/
  Cargo.toml
  README.md
  AGENTS.md
  docs/
    architecture.md
    source-audit-v4.7.3.md
    compatibility-profile.md
    endpoint-manifest.md
    domain-contracts.md
    visibility-and-authorization.md
    application-ports.md
    persistence-conformance.md
    federation-contract.md
    streaming-contract.md
    testing.md
    phase-0-exit-criteria.md
    logs/                         # append-only verification evidence
    wiki/
  src/
    main.rs
    lib.rs
    config.rs
    observability.rs
    domain/                       # knows neither HTTP DTOs, ActivityPub JSON, nor redb
      account.rs / user.rs / status.rs / visibility.rs / interaction_policy.rs /
      quote.rs / relationship.rs / follow.rs / block.rs / mute.rs / favourite.rs /
      bookmark.rs / list.rs / notification.rs / poll.rs / media.rs / filter.rs /
      id.rs / error.rs
    application/                  # use cases; depends only on domain + ports
      ports.rs / context.rs       # dependency assembly only
      accounts/ / auth/ / statuses/ / quotes/ / relationships/ / timelines/ /
      notifications/ / polls/ / media/ / search/ / lists/ / filters/ / instance/ /
      scheduled_statuses/
    api/                          # adapters: HTTP + OAuth
      mastodon/
        router.rs / auth.rs / errors.rs / request.rs / pagination.rs / rate_limits.rs
        dto/ / endpoints/
      oauth/
        authorization.rs / token.rs / scopes.rs / pkce.rs / consent_page.rs
    activitypub/                  # ActivityPub adapter
      actor.rs / objects.rs / parser.rs / serializer.rs / webfinger.rs /
      signatures.rs / inbox.rs / outbox.rs
    federation/                   # port/adaptation layer to `activitypub-federation-rust` (resolver, delivery, retry, dedup, remote media)
    streaming/                    # hub, auth, channels, SSE, WebSocket
    jobs/                         # durable job queue + workers
    media/                        # ingest, blob store, processing, remote cache
    persistence/
      redb/                       # single storage adapter behind ports
        mod.rs / schema.rs / accounts.rs / statuses.rs / relationships.rs /
        quotes.rs / polls.rs / media.rs / notifications.rs / oauth.rs / jobs.rs / migrations.rs
  tests/
    api_contract/ / oauth/ / persistence_conformance.rs / crash_recovery.rs /
    federation_interop/ / streaming/ / client_smoke/
  scripts/
    smoke-all.sh / bench-local.py
```

Dependency direction: `domain` ← `application` ← adapters. No API handler opens a `redb` table directly; all storage goes through narrow ports in `application/ports.rs`. Router assembly is declarative; endpoint files stay small (avoid pinetree's oversized `mod.rs` growth pattern).

Atomic mutation protocol (per `birchtree.md` §5):
1. Parse input and do slow/remote lookups before the write transaction when safe.
2. In one short transaction, re-check mutable invariants and commit primary records, indexes, local counters, dedup/idempotency records, and bounded durable jobs together.
3. Process media conversions, notifications, streaming publication, and outbound federation after commit. Every worker is idempotent and settles its lease by success, retry, defer, or terminal quarantine.
4. Crash tests prove no committed mutation loses its required job; failure before commit leaves neither mutation nor job.

Media bytes, HTTP calls, follower lists, and unbounded fan-out never enter a transaction.
