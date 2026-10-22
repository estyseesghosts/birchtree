# Architecture

> Status: Draft. For future implementers. Sources: `src/domain/*`, `src/application/*`, `src/persistence/redb/schema.rs` + `migrations.rs`, `src/config.rs`, `src/lib.rs`, `src/main.rs`, `src/jobs/*`, `docs/architecture.md`, `docs/database.md`, `docs/persistence-conformance.md`, `docs/domain-contracts.md`, `docs/application-ports.md`, audit `1021-birchtree-audit.md` §§1.1–1.7. Code facts are at time of audit (SCHEMA_VERSION 21). Where behavior is unverified it is marked **Open question**.

## 1. Whole-system map

### 1.1 Process and crate graph (one modular monolith, one Cargo package)

```
                         +---------------- birchtree binary (src/main.rs) ----------------+
                         |                                                                |
                         |  config_base() -> load JSON (--config=) -> CLI overrides ->     |
                         |    validate() -> check_runtime_mode() -> startup_summary log    |
                         |                                                                |
  operators/CI           |  build_store() -> RedbStore::open(birchtree.redb)              |
  create-account ------->|    highest_persisted_id() -> ids.observe() (SKEW_WARN 60s)      |
  grant-role             |  FsBlobStore::open_pair(durable, cache) -> PooledBlobStores     |
  create-rule/*          |  CacheMaintenance::restore_at_startup + seed_jobs               |
  sweep-transcode-*      |  Hub::new(store, clock, server, Limits::default)                |
  crash-harness  ------->|  federation build_config_with_remote_cache (SafeHttpConfig)    |
                         |  ApiState{context, trusted_proxies, server, streams, ...}      |
                         |  build_router_with_probes(state, RuntimeProbes)                |
                         |  jobs::worker::run (...) [tokio task] + axum::serve + graceful  |
                         |    shutdown (SIGINT/SIGTERM -> shutdown watch -> worker drain)  |
                         +----------------------------------------------------------------+
                                            |                |                |
                       HTTP :3000 -----------+                |                +--- in-process only
                         (axum 0.7)                           |                     (no Redis/Node/PG)
                                                              |

  HTTP request path:
  TCP -> rate_limits::middleware -> json_body::guard -> ip_block_middleware
       -> vary_authorization -> DefaultBodyLimit(max_body_size) -> TraceLayer
       -> router.rs (~200 method+path rows + activitypub::http::routes)
       -> auth.rs (Bearer/query/sec-websocket-protocol) -> scope/role gates
       -> endpoints/* -> application::<use case> -> RedbStore (ports)
       -> ONE redb write txn (state+indexes+counters+dedup+jobs)
       -> 200/201/202/206 + Link + X-RateLimit-* (+ Vary: Authorization on /api/*)
       -- after commit only: jobs worker -> streaming Hub -> federation delivery
                             -> media transcode -> search reindex -> mail SMTP

  Storage:
  birchtree.redb (single redb file, SCHEMA_VERSION 21, ~90 tables, §5)
  media_durable/  (<data_dir>/media, never evicted)  } FsBlobStore::open_pair
  media_cache/    (<data_dir>/media_cache, evictable) } PooledBlobStores routing

  Background plane (all in-process tokio tasks/threads):
  jobs::worker::run --claim_due--> handlers / delivery::run / events::run
      / threads::{resolve_parent, crawl_replies} / media::fetch / preview::fetch
      / push run_push_job_record / transcode::run_transcode_job (spawn_blocking)
      / CacheMaintenance::handle_job (CacheEvict, ReconcileStorage)
      --settle--> store.complete | store.retry (backoff) | store.quarantine

  Fan-out plane (post-commit, never in txn):
  application use case --publish StreamEvent--> streaming::Hub (mpsc sync_channel)
      --fanout thread--> fanout::process (re-reads durable state) --> SSE / WS
  application use case --enqueue FederationEvent--> DeliverActivity per inbox
      --> SafeHttp client (SSRF/timeout/redirect/size) --> remote inbox
  application use case --enqueue SearchIndexStatus/SearchBackfill/TagBackfill
      --> search::EmbeddedIndex (in-process inverted index, derived state)
  application use case --enqueue ProcessMedia/TranscodeMedia
      --> media::{processing, transcode} (ffmpeg) --> blob pools --> Ready
```

### 1.2 Module tree (what lives where)

```
src/lib.rs: pub mod activitypub, api, application, config, domain, federation,
                  jobs, mail, mailer, media, observability, persistence, search, streaming
src/main.rs: config_base, parse_args, build_store, serve, run_create_account,
             run_grant_role, run_crash_harness, load_secret_box, shutdown_signal,
             CLI: create-account/grant-role/create-rule/create-text/rule-translation/
                  list-rules/crash-harness/sweep-transcode-orphans
src/config.rs: Config, RemoteCacheConfig, RemoteCacheMode, RuntimeMode, StartupSummary
src/observability.rs: init(), redact(), StorageMetrics/STORAGE_METRICS
src/domain/ (30 files, no redb/axum/ActivityPub imports):
  account, user, status, visibility, interaction_policy, quote, relationship,
  follow, block, mute, favourite, bookmark, list, notification, poll, media,
  filter, marker, scheduled_status, trends, custom_emoji, federation, id,
  idempotency, job, oauth, conversation, report, rule, admin_infra,
  annual_report, error, mod.rs
src/application/ (flat modules; NOT birchtree.md §3 accounts/ subdirs):
  ports.rs (all Store traits), context.rs (Context assembly only),
  accounts, auth, statuses, quotes, relationships, interactions, policy,
  timelines, lists, filters, markers, media, notifications, polls, search,
  streaming, federation{,_inbound,_ports}, push{,_crypto}, admin{,_infra},
  announcements, annual_reports, collections, conversations, crypto, emoji,
  instance, scheduled_statuses, suggestions, trends, web
src/api/mastodon/: router.rs (+ router/readiness.rs), auth.rs, errors.rs,
  request.rs, pagination.rs, rate_limits.rs, dto/*, endpoints/* (~40 files)
src/api/oauth/: authorization.rs, token.rs, scopes.rs, pkce.rs, consent_page.rs
src/api/: forms.rs (LenientForm/FormBody), json_body.rs (body_pairs + guard)
src/activitypub/: actor, objects, parser, serializer, webfinger, signatures,
  inbox, outbox, activities, outbound, outgoing, wire, http, instance
src/federation/: resolver, delivery, retry, dedup, net, policy, events,
  threads, media, preview, remote_cache
src/streaming/: hub.rs (Hub+Limits), auth.rs, channels.rs, sse.rs,
  websocket.rs, render.rs, fanout.rs, mod.rs
src/jobs/: types.rs (re-export), queue.rs (re-export consts), worker.rs (run),
  handlers.rs, search_index.rs, tag_backfill.rs, transcode.rs, account_deletion.rs
src/media/: ingest.rs, blob_store.rs (FsBlobStore), processing.rs,
  remote_cache.rs, transcode.rs, fetch_stream.rs, reconciler.rs,
  cache_maintenance.rs, pooled_blob_stores.rs, storage_roots.rs
src/persistence/redb/: mod.rs (RedbStore+metrics), schema.rs (TableDefinitions),
  migrations.rs (v1..v21), accounts, statuses, relationships, quotes, polls,
  media, notifications, oauth, jobs, timelines, admin, ... + secrets.rs
src/search/: mod.rs (SearchMode), embedded.rs, backfill.rs
src/mail.rs (LoggingMailSender, SmtpJobSender), src/mailer/*, tests/*
```

### 1.3 Data-flow examples end to end

* `POST /api/v1/statuses` → `endpoints/statuses` parses form-or-JSON (`media_ids[]`, `poll[options][]`, `Idempotency-Key`) → `application/statuses::create_status` validates (visibility, `can_fetch` on parent, quote policy, media ownership) → single `write_status_txn` commits `STATUSES` + `STATUS_AUTHOR_INDEX` + `STATUS_PARENT_INDEX` + `MENTIONS_BY_ACCOUNT` + `HASHTAG_INDEX` + `TIMELINE_PUBLIC/LOCAL` + `POLL*`/`QUOTE*` + `IDEMPOTENCY` + `ACCOUNT_STATS` counters + `FanOutNotification` + `FederationEvent` + `SearchIndexStatus` jobs → returns `Status` → endpoint packs `dto::Status` → `streaming.publish(StatusCreated)` after `Ok`.
* `POST /api/v1/media` → multipart streamed into capped spool (`max_file_size`, default 16 MiB) → `media::ingest::sniff` decides type → bytes staged in `FsBlobStore` *before* commit → `create_media_upload` commits `MEDIA` + `MEDIA_BY_OWNER` + `ProcessMedia` in one txn → v1 sync 200 or v2 202 (`Pending`); worker `ProcessMedia`/`TranscodeMedia` (ffmpeg off-thread) → `mark_media_processed` → `Ready` (`url` non-null) or `Failed` (422 on show).
* `GET /api/v1/timelines/home` → `timeline_query` (limit/cursors) → `ViewerScope` seed (2 store calls) → `Store::authors_timeline` bounded k-way merge → `StatusViewContext` batched page-load → `filter_results_scoped` applies keyword/status rules → DTO pack → headers `private, no-store`, `Link`, `Vary: Authorization`.
* Inbound `POST /inbox` → vendor `verify_body_hash` + `verify_signature` (+1 forced refetch on stale key) → `activities::*::receive` → `application::federation_inbound` → one txn (records + `ACTIVITY_DEDUP` + follow-up jobs) → 200/202; `Unsupported` type → `202 accepted-and-dropped` (D32).

## 2. Dependency direction: `domain <- application <- adapters`

Rule: domain knows neither HTTP DTOs, ActivityPub JSON, nor `redb`. No handler opens a `redb` table. REST and ActivityPub call the same application use cases.

### What may call what

| Layer | May import | Must not import |
|---|---|---|
| `domain::*` | `std`, `serde`, `thiserror` only | `redb`, `axum`, `activitypub_federation`, `api::dto` |
| `application::*` | `domain::*`, `application::ports` traits | `redb::`, `begin_write`, axum extractors |
| adapters (`api`, `activitypub`, `federation`, `streaming`, `media`, `jobs`, `persistence`) | application use cases + ports | duplicate business logic per adapter |

REST handlers call `application::statuses/timelines/relationships`; ActivityPub ingress calls `application::federation{,_inbound,_ports}`. No duplicate follow/status/favourite logic. `persistence::redb::*` implements `application::ports` traits assembled in `application/context.rs::Context`. `streaming::Hub` implements `StreamingSink`; `FsBlobStore` (+ `PooledBlobStores`) implements `BlobStore`.

Known violations (fix, do not copy): `application/filters.rs:21` imports `api::dto`; `application/media.rs:729` (test) imports `RedbStore`.

## 3. IDs, pagination/cursors, error envelope

* Snowflake `Id(u64) = (timestamp_ms << 16) | sequence`. Wire: decimal string. Ordering numeric (never lexicographic). Generation is a deterministic counter (single global generator; two processes on one DB unsupported). `observe(persisted)` floors the counter at startup; skew ≥ 60s logs warn.
* Storage keys are `u64` / `desc_id = u64::MAX - id` (forward scan = newest first), 16-byte pairs, `0xFF`-separated composites.
* Pagination: `parse(limit, max_id, since_id, min_id)`; non-numeric cursor → 400; negative limit → 400; `limit=0` → caller default. `Link: next=max_id, prev=min_id` on non-empty pages only; non-cursor query params are echoed (D13).
* Errors: `{"error":"..."}` everywhere (`404 Record not found` vs explicit `404 Not Found` on status sub-routes, `422` validation, `401` token, `403` scope/role, `409` marker conflict, `413` body cap, `429 + Retry-After + X-RateLimit-*`). OAuth token failures use Doorkeeper `{"error","error_description"}`.

## 4. Mutation protocol, jobs queue, crash recovery

1. Parse input and do slow/remote lookups *before* the write txn.
2. In one short `redb` write txn, commit primary records + affected indexes + local counters + dedup/idempotency records + bounded durable jobs.
3. Media conversions, notifications, streaming publication, outbound federation happen *after* commit via idempotent workers (success / retry with backoff / quarantine).

Jobs: `Job{id, kind, payload, state: Pending|Leased|Succeeded|Quarantined, due, lease_until, attempts (max 5), last_error}`. 27 kinds (`user_created`, `send_confirmation_email`, `deliver_activity`, `federation_event`, `resolve_parent`, `crawl_replies`, `fetch_remote_media`, `transcode_media`, `account_deletion_batch`, ...). Scheduling index key = 16-byte `(due||id)` while pending, `(lease_expiry||id)` while leased. `LEASE 30s`, backoff `1s << attempts` capped at 300s. Worker `DEFAULT_BATCH=32`. Delivery tuning: per-host cooldown base 60s doubling cap 1h, failure budget 10, max 25 deferrals (~1 day tail, D49).

Crash safety: single-writer `redb`; races resolve to one winner; `kill -9` tests prove committed mutation + job survive and aborted opens leave nothing (`tests/crash_recovery.rs`). Media crash safety: ffmpeg → temp outside store → staged sibling `derived.{webm,webp}` → verified → overwrite own `file_key` → row `Ready` last.

## 5. Storage: redb tables, keys, migrations, blob pools, media lifecycle

### Table groups (`schema.rs`, SCHEMA_VERSION 21)

Identity/auth: `META, ACCOUNTS, HANDLE_INDEX, LOCAL_USERS, OAUTH_APPS, ACCESS_TOKENS, SESSIONS, AUTHORIZATION_CODES, EMAIL_INDEX, EMAIL_TOKENS, INVITES, INVITE_USES, TOTP_SECRETS, WEBAUTHN_*`. Statuses/threads: `STATUSES, STATUS_BY_URI, STATUS_AUTHOR_INDEX, STATUS_PARENT_INDEX, STATUS_REBLOG_INDEX, REBLOGS_BY_ACCOUNT, STATUS_VERSIONS, STATUS_PINS, REMOTE_REFRESH, REMOTE_TOMBSTONE, UNRESOLVED_PARENT, REPORTS`. Relationships: `FOLLOWS(+BY_FOLLOWED), BLOCKS(+BY_BLOCKED), DOMAIN_BLOCKS(+BY_ID), MUTES(+BY_MUTED), ACCOUNT_NOTES, ENDORSEMENTS, FAVOURITES(+BY_STATUS), BOOKMARKS(+BY_STATUS), ACCOUNT_STATS, IDEMPOTENCY, INTERACTION_EVENTS, MENTIONS_BY_ACCOUNT, HASHTAG_INDEX`. Polls/quotes: `POLLS, POLL_BY_STATUS, POLL_VOTES, QUOTES, QUOTES_BY_QUOTED, QUOTES_BY_QUOTING, QUOTE_BY_APPROVAL`. Notifications (+dedup/policies/requests). Media (+by-owner/remote-source/remote-cache/accounting). Timelines: `TIMELINE_PUBLIC, TIMELINE_LOCAL`. Lists/members, markers, filters(+keywords/statuses), scheduled(+by-account), emoji(+remote). Federation: `ACTOR_KEYS, ACCOUNT_BY_ACTOR_URI, ACTIVITY_DEDUP, FOLLOW_BY_URI, DELIVERY_DEST`. Conversations/push/web/tags, admin/domain/email/IP/canonical/tags/audit, trends/annual, jobs(+index), announcements, collections(+items).

### Migrations v1–v21

Versioned `if current < N`, one txn, restartable/idempotent, newer-than-build refused. v1 identity/jobs; v2 auth codes; v3 statuses/relationships/polls/quotes/notifications/media; v4 timelines/lists/markers/filters/scheduled/emoji + public/local backfill; v5 domain-block ids + backfill; v6 actor keys/dedup/follow-URI + actor-URI backfill; v7 refresh/reports/remote-source/quote-approval/unresolved-parent + status-URI backfill; v8 admin/audit/trends/annual; v9 conversations/push/web/tags; v10 tombstone/delivery-dest; v11 rules/warnings/sign-ins; v12 invites/email-index/tokens/interaction-events; v13 suggestion-mutes; v14 announcements; v15 invite-uses; v16 `QUOTES_BY_QUOTED` rekey; v17 remote-cache skeletons; v18 collections/TOTP/WebAuthn; v19 rule translations; v20 remote emoji; v21 WebAuthn challenges.

### Blob pools and media lifecycle

Two `FsBlobStore` roots: durable `<data_dir>/media` (local uploads, never evicted) and cache `<data_dir>/media_cache` (remote fetches, evictable). Atomic tmp+rename writes, `0700` dirs, key validation, `put_streamed` caps, `publish_tmp` staging. States `Pending|Processing|Ready|Failed`; `url: null` + `202`/`206` while not ready. Upload validation: blank/sniff/oversize/description/focus/thumbnail rules. Transcode (Phase 7.1): GIF→WebM/VP9 `video`, video→WebM/VP9+Opus, stills→WebP, never `gifv` (D1–D4); 120s wall, 64MiB output, 1GiB RSS; missing ffmpeg with transcoding on = startup error. Remote media: metadata-only ingest → `FetchRemoteMedia` on first request → signed GET → 40MiB cap → per-row `remote/<id>` + ledger reserve/commit → `ProcessMedia` → `Ready`.

## 6. Timelines: bounded fan-out-on-read

No Redis materialization. Bounded k-way merge over followed-author ranges; rebuildable `redb` feed index deferred until benchmarks. Caps: `HOME_AUTHOR_CAP 1000`, `LIST_MEMBER_CAP 500`, `ACCOUNT_CACHE_CAP 2000` (fail-closed), `PUBLIC/HASHTAG_SCAN 2000`, home/list merge `5000/600`, multi-tag cap 4. `ViewerScope` lazy-loads relationship state in 2 store calls; `Home/Public/Tag/ListFilter` enforce visibility; page loaders batch (`accounts_by_ids`, `batch_pair_states`, ...). No home `206`; read-time ancestor completion for logged-in context reads (D5). Retraction pages `authors_timeline` 500/batch × 64/author-set (D15); pre-commit scan fan-out is a Phase-8 load item.

## 7. Cross-cutting

Config (`config.rs`): JSON file + CLI flags + `validate()` + `check_runtime_mode()`; data dir `0700`; production refuses weakened settings without exact override phrase. Observability: `tracing`, `RUST_LOG`, secret redaction, storage metrics. Health `/health`, readiness `/ready` (2s deadline, failing names logged not returned), loopback-only `/metrics`. Rate limits: bounded in-memory fixed windows, per-process; general 1500/5min + per-route sensitive budgets; trusted-proxy-aware client IP (`X-Forwarded-For` honored only from trusted peers; IPv6 /64). Request parsing: form-or-JSON (D11) with Rails-like flattening, multipart capped spool, `key`+`key[]` arrays, nested `poll[]`/`fields_attributes[]`/`keywords_attributes[]`.

## Open questions

`can_fetch` vs `can_fetch_full` split; `limited` runtime rule; `can_deliver_to_inbox`; suspended/silenced fetch gate; streaming gaps; BMP decoder pin; MIME reconciliation; video cap knob; federation emit decisions; `include_filtered` wiring; v2 grouping; poll audience; `Move` sender; collection sync; Phase-8 perf (fan-out thread, retraction scans, per-status lookups, tag-merge reads).
