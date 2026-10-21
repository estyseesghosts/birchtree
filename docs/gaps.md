Status: Draft

# birchtree gaps — kill-list (database.md assumed done)

Parent reads first: `birchtree.md` §§2–5,7, `docs/remote-media-cache-plan.md` §3 (frozen: per-row keys, no sharing), `docs/database.md` (done: two pools, on-demand cache, stream-to-disk, reconciler), `docs/TODOs.md`, `docs/compatibility-profile.md` D35/D43/D45–D52, `AGENTS.md` delegation + config + state rules, latest `state/logs/`. `git status --short` clean before start. Subagents leave changes uncommitted; parent merges (shared files parent-only, never subagent-edited).

Explicitly out (not this batch): `docs/TODOs.md` verification-debt section (suites, parity suite, prom endpoint, Phase 8 perf, contract freeze, source-audit review, scheduled-announcement job, `indexable_statuses_page`, tantivy, cap-conflation) and post-parity programs (Aperture/Clips/Antennas/Pages). Deliberate non-goals stay non-goals (`translate`, `gifv`, reload, web UI, FASP, Postgres/Redis/cluster, `oembed`, `v1` search).

## Frozen decisions (kills the operator-decision queue)

* Password reset: server-rendered `GET /auth/password/edit?reset_password_token=` page + `POST` set-password port (Devise-shaped). 6h `PasswordReset` bearers, consume-on-write, unknown/used indistinguishable 422. No new mailer shape.
* Suggestions: honest local signals only (friends-of-friends, then most-followed). Directory always on. No staff/FASP/remote signals.
* Site texts: CLI + file authoring (`create-text/--site-terms-file/--site-privacy-file/--site-extended-file`), served by instance sub-resources instead of 404. No web authoring UI.
* First-party: `/api/v1/*` uses OAuth tokens (proposed default). Session stays login/consent-only.
* Media caps: per-file `max_file_size` only (existing) + status `500/--max-status-chars 3000`. No per-user quota (would be a new `D` divergence; stays deferred with `database.md`).

## Parent-owned shared files (subagents propose snippets, never edit)

* `docs/endpoint-manifest.md`, `docs/compatibility-profile.md`, `docs/TODOs.md`, `docs/testing.md`
* `src/application/ports.rs`, `src/persistence/redb/schema.rs` + `mod.rs` + `migrations.rs`
* `src/config.rs` + `src/main.rs` + `config.example.json`
* `src/api/mastodon/router.rs`, `src/domain/job.rs`, `src/jobs/worker.rs`
* `tests/persistence_conformance.rs`

Parent Step 0 lands skeletons only: `SCHEMA_VERSION+1` with `COLLECTIONS{…}`, `COLLECTION_ITEMS{…}`, `TOTP_SECRETS{…}`, `WEBAUTHN_CREDS{…}` tables (names only); `JobKind`: `QuoteReverify`, `CollectionFanout`, `RelayForward` (payloads <64 KiB, batches 500, no blobs in txn); config keys from decisions above (all optional, `validate()`, round-trip test, `deny_unknown_fields` stays).

## Agent partition (disjoint writes)

* **G-A — operator + profile surfaces.** Owns: `src/api/mastodon/endpoints/instance.rs` (site texts, `translation_languages`, `languages` ISO-639 names), `src/api/mastodon/endpoints/accounts.rs` (reset page port wiring, `GET/PATCH /profile`, avatar/header DELETE), CLI text/rule commands in `src/main.rs` (snippet only), `tests/gaps_profile_surfaces.rs` (new). Tasks: reset page + set-password; site texts serve real content; rule `translations` store (fallback `""`); locale catalog; `profile` read/update parity with `update_credentials`; `identity_proofs=[]`, `email_subscriptions` no-op shape per upstream. Proposes snippets for router/ports/schema.
* **G-B — notifications + streaming completeness.** Owns: `src/application/notifications.rs`, `src/streaming/fanout.rs` + `hub.rs` (announcement events only), `tests/gaps_notifications.rs` (new). Tasks: real producers for all D51 types (`status/update/severed_relationships/moderation_warning/annual_report/admin.sign_up/admin.report/quoted_update/added_to_collection/collection_update`) wired to their features; `announcement/announcement.reaction/announcement.delete` stream events; `include_filtered` + `supported_types` honored (filtered rows served, fallback rendering built); backfill `Unsubscribe-URL` on pre-token push rows (send-or-mark, never fabricate). D51/D52 rows closed with evidence.
* **G-C — collections + missing REST surface.** Owns: `src/api/mastodon/endpoints/collections.rs` (new), `src/persistence/redb/collections.rs` (new), `src/application/collections.rs` (new), `tests/gaps_collections.rs` (new). Proposes snippets for router/ports/schema. Tasks: top-level `collections` CRUD + `items` + `revoke`, `in_collections`, account `lists` sub-resource, `v1_alpha` namespace (minimal), `domain_blocks/preview`, `peers/search`, `GET /api/v2/notifications` family (index/clear/unread/dismiss/`/:group_key`/accounts/policy); `hide_collections` enforced (members hidden, counts stay per D30); `reblogged_by` `distributable_visibility` scope. All rows in manifest; no stubs.
* **G-D — federation tail.** Owns: `src/activitypub/activities.rs`, `src/activitypub/objects.rs`, `src/federation/threads.rs` + `delivery.rs` + `media.rs` (tail arms only), `tests/gaps_federation_tail.rs` (new). Proposes snippets for inbox routes, ports/schema. Tasks: Move (+migrated follows, individual `D` rows), remote emoji ingest (arrays carry remote emoji), relays/inbox-forwarding (reachability claims), follower sync + remote counters, Note/featured collections, quote re-verify workers + remote counts, poll `collection_update` notifications, report→`Flag` forward to remotes, live WebFinger resolve (unknown remotes fetched, 404 only when truly missing), `GET /api/web/embeds/:id` remote fetch+sanitize. D32 refuse-stands narrows to truly-unknown only; per-piece `D` rows added.
* **G-E — auth + moderation hardening.** Owns: `src/domain/user.rs`, `src/application/accounts.rs` (auth arms only), `src/api/mastodon/auth.rs` (gates only), `src/api/mastodon/endpoints/admin_accounts.rs` (perm arms only), `tests/gaps_auth_moderation.rs` (new). Tasks: TOTP (+ backup codes) and WebAuthn second factor; `time_zone`/`date_of_birth` capture + min-age policy gate; IP-block client-IP verification test across proxy shapes; email-domain/canonical-email block enforcement reconciled with confirmation redemption; D35 fine-grained per-action flags on `Admin::BasePolicy` model (moderator scope widens per flag, audit per write); role serializer numeric ids + real bitmask everywhere (`-1` gone).

## Verification gate (parent runs)

```bash
cargo check --all-targets
cargo test --test gaps_profile_surfaces --test gaps_notifications --test gaps_collections --test gaps_federation_tail --test gaps_auth_moderation
cargo test
bash scripts/smoke-all.sh
```

Plus: reset-link end-to-end (mail → page → login); site-text/authorship round-trip; filtered-notif + announcement-event socket capture; collection CRUD + federated Add/Remove round-trip; live-WebFinger + Move + relay lab run; TOTP/WebAuthn login + min-age refuse + IP-block true-client test. Paste real output into `state/logs/YYYY-MM-DD-gaps.md`, update `TODOs.md` + manifest + `D` register in place, never rewrite old logs.

## Finish decisions (parent, 2026-10-21)

* `severed_relationships`: recorded as a gap, not wired. No sever cascade
  exists (blocks sever endorsements only; domain blocks sever nothing), so
  there is no honest trigger transaction. The builder is real and the empty
  observable outcome is pinned by test. Trigger condition for a future
  batch: button up a sever cascade first, then wire the emit at its commit.
* `email_subscriptions`: not a gap. No pinned public-API shape was found
  (G-A source check of the methods index + pinned routes); it stays 404
  out-of-profile rather than a fabricated endpoint.
* Relay fan-out narrowed to public-visibility content events
  (`create`/`update`/`delete`/`announce`/`question_update`): targeted
  traffic (likes, flags, quote requests, blocks, profile/deletion
  bookkeeping) never leaves addressed inboxes. Stricter than the proposed
  snippet; privacy-protective.
* `featuredTags` collection advertised on local Persons only (remotes carry
  none, like followers/following/outbox).
* Follower sync = counts-only `SyncFollowers` refresh (followers/following/
  outbox `totalItems`, absurd-filtered, remote rows only); follow rows stay
  activity-driven. Triggered on live-WebFinger resolve + account Move.
* `minimum_age_years` plumbed end to end (config → `ServerInfo` →
  registration use case; default 16); WebAuthn challenges durable
  (`WEBAUTHN_CHALLENGES`, schema v21); email-domain signup check is one
  targeted index read (domains are unique at creation).
* Schema ledger: v18 collections + TOTP/WebAuthn credential tables, v19
  rule translations, v20 `REMOTE_EMOJI`, v21 `WEBAUTHN_CHALLENGES`.
* Job ledger: `FetchRemoteEmoji` (worker-owned) and `SyncFollowers`
  (worker-owned) added; `CollectionFanout` runs in the sync drain.
