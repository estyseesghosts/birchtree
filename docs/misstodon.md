# Misstodon: clips, antennas & pages for birchtree (config-gated modules, bookmarks-as-clips, federated with pinetree)

Status: plan (build after Batch Q lands; re-verify the starting-state list at build time).

## Why

Pinetree proved the domain/storage shape for clips and pages (`~/pinetree/src/domain/{clip,page}.rs`, same ports→redb→conformance discipline as here). Birchtree needs the same *capabilities* but through its own surface: the Mastodon client API, which has no clips, no antennas-as-collections, and no pages. So this is a port of the subsystems, not of pinetree's wire format — with one structural difference from pinetree: **all three are optional modules the core does not depend on**, toggled by JSON config flags (default off), because Mastodon clients cannot understand them.

The one exception is bookmarks: Mastodon *does* understand bookmarks, and they already exist as a core feature. Rather than run two private-collection systems, **bookmarks become a private, unrenamable system clip**. The Bookmarks folder then behaves identically whether clips are later enabled or not, and the MastoAPI keeps working either way.

**Federation is v1 scope, not deferred** (owner decision): birchtree clips and pages federate with pinetree using pinetree's wire shapes (`OrderedCollection` + `Article`, `Add`/`Remove`/`Update`/`Delete`/`Like`), since the formats are implementation-agnostic by design. Antennas stay local-only (no peer implementation exists — pinetree antennas are local-only too).

## Canonical URIs (locked now — URLs are forever, everything else is additive)

These go live the moment the modules do; changing them later breaks every federated reference. No other part of this plan constrains them:

- Clips (collection + pages): `https://{domain}/clips/{decimal-id}`, with `?page=true` collection paging mirroring pinetree's envelope (`OrderedCollection` index with `first`, `OrderedCollectionPage` with `orderedItems`, `partOf`).
- Pages: `https://{domain}/pages/{decimal-id}`, serving the `Article`.
- `attributedTo` is always the owner's actor URI (existing WebFinger/actor infra).

## What "module" means here (the core decision — read this first)

A module is a **leaf**: new files only, in new directories, depending inward on nothing but domain types, ports, and shared helpers. The dependency rule (`domain` ← `application` ← adapters) is unchanged; the core never imports module code. Concretely:

- **New code lives in new files.** `domain/clip.rs`, `domain/antenna.rs`, `domain/page.rs` (plus `ClipKind`); `application/clips/`, `application/antennas/`, `application/pages/` (ops) or one file each if small like the list/filter areas — decide at build, same pattern either way; `api/mastodon/endpoints/{clips,antennas,pages}.rs`; store traits in `application/ports.rs` next to the existing `Store` area; tables in `persistence/redb/` next to the per-area files.
- **The flag gates the surface, not the storage.** `clips_enabled`, `antennas_enabled`, `pages_enabled` (bool, default `false`) are read from `Config` → `ServerInfo::from_config` (same path as `trends_enabled`). The router mounts that module's routes **only when its flag is on**; when off the routes don't exist (unknown-route 404, the honest out-of-profile answer per AGENTS.md — never a success-shaped stub). Handlers additionally re-check the flag (defense in depth against future route-table edits).
- **What "birchtree does not depend on it" means in practice:** switch every flag off and the server builds, boots, and passes the full suite with zero behavioral delta — the only observable difference is 404s on the module routes and no module keys in the instance advertisement. No core MastoAPI path may import module code; module code may import core. Test doubles keep working because ports gain methods, never change existing signatures (same discipline as prior batches).
- **No Cargo features in v1.** One binary, per the modular-monolith rule in both repos' AGENTS.md. Compile-time stripping is an explicit non-goal (follow-up at most); the user asked for JSON flags, and flags deliver the requirement with none of the feature-unification hazard.
- **No new CLI flags required.** File-only keys, following the `trends_enabled` / `peers_api_enabled` / `domain_blocks_public` precedent ("No CLI flag; file-only"). Every key documented in `docs/configuration.md` + `docs/config-schema.json` + root `config.example.json`, with round-trip cases in `tests/config_file.rs` (which fails on schema drift — keep it green).

## Phase 0 — reconcile + inventory (read-only first, no code)

1. Confirm Batch Q landed and nothing below collides with it (especially Q's federation trio and any new ports/tables touching `Store`, `schema.rs`, or the inbox dispatch).
2. Verify the starting-state list in this doc against HEAD (bookmark paths, `Id` rules, instance DTO builders, free conformance naming).
3. A subagent diffs `~/pinetree/src/domain/antenna.rs` (and clip/page field lists) against what's specified below and reports the exact port list — pinetree is the reference implementation, not a dependency; no pinetree code is copied wholesale, only semantics.
4. Output: a short superseding note appended here if anything changed.

## Phase 1 — shared foundation (once, used 3×)

- **Config**: `clips_enabled`, `antennas_enabled`, `pages_enabled: bool` (default `false`) on `Config` + mirrored `ServerInfo` fields via `from_config`; `config-schema.json` entries; `configuration.md` section; `config.example.json` entries; `tests/config_file.rs` round-trip cases. `validate()` needs no new rules (bools); state that explicitly.
- **IDs**: all new rows use `domain::Id` (64-bit timestamp-oriented, decimal strings on the wire, numeric comparison internally — AGENTS.md hard rule). Never lexicographic.
- **Ports**: `ClipStore` (incl. membership + favorites + system-clip lookup by `(owner, kind)` + clip tombstones), `AntennaStore` (+ matched-feed reads), `PageStore` (+ page likes + featured query) in `application/ports.rs`, following the existing `Store`-area conventions (one txn commits mutation + indexes + bounded jobs). Ports never change existing signatures (test doubles keep compiling).
- **Storage**: redb tables + per-area files mirroring `statuses.rs`/`accounts.rs` layout; conformance tests alongside the flat tests in `tests/persistence_conformance.rs` (+ `persistence-conformance.md` note, no lettered groups in this repo — follow the file's existing flat-`#[test]` convention).
- **Errors/scopes**: Mastodon-shaped errors (`ApiError::not_found()` / 422 validation via the `list_error` pattern — **not** Misskey UUIDs; that's pinetree's world). New OAuth scopes `read:clips`/`write:clips` (+ antennas/pages equivalents), documented as birchtree extensions; DTO packers in `dto/`.

### Domain shapes (ported from pinetree, adjusted — subagent diffs `~/pinetree/src/domain/{clip,page,antenna}.rs` in Phase 0 and reports deltas)

- `Clip {id, owner_id, name≤100, description?, is_public (default false), tier, federate (default false), approved_actor_uris, kind, created_at_ms, last_clipped_at_ms}` + `ClipNote {id, clip_id, status_id, created_at_ms}` (note: *status*, not note — birchtree vocabulary) + `ClipFavorite {id, user_id, clip_id}`. `CollectionTier` ported wholesale (`Private → LocalApproved → Unlisted → Listed → RemoteApproved`, same semantics as pinetree — identical rules both sides is what makes the interop matrix testable 1:1). Tier defaults from `is_public` (`true` ⇒ `Listed`, else `Private`). Bookmarks clip: `is_public=false`, tier `Private`, `federate=false` (never served; MastoAPI reads the memberships directly at the application layer, bypassing the gate).
- `Page {id, owner_id, title ≤256, name slug (unique per owner), summary?, content (blocks), variables, script (stored, never executed, never federated), eyeCatchingImageId? (must be own file), font/alignCenter/hideTitleWhenPinned, visibility, visible_user_ids, tier, federate (default ON — mirrors the pinetree owner decision), approved_actor_uris, uri (canonical remote URI, `None` = local), likedCount, created_at/updated_at}`. Two axes, distinct jobs: **`visibility`** (birchtree `Visibility`: `Public`/`Unlisted`/`Private`/`Direct` + ids) governs **local reads**; **`tier`** (ported `CollectionTier`) governs **remote serving** via the fetch gate. Local creates are always `Public` (upstream parity; no UI to change it). Inbound mapping sets both: Public in `to` ⇒ (`Public`, `Listed`); Public only in `cc` ⇒ (`Unlisted`, `Unlisted`); followers collection ⇒ (`Private`, `Private`); otherwise (`Direct` + best-effort-resolved ids, `Private`). Non-Listed tiers are never advertised and never fan out.
- `Antenna` ports pinetree semantics verbatim (keyword groups, exclude groups, src set, users/list sources, case-sensitivity, localOnly, excludeBots, withReplies, withFile) with the `Id` swap.
- **No separate `visibility` on Clip**: local reads use `is_public` (upstream Misskey parity); remote serving uses `tier`. The two axes stay distinct on purpose.

## Phase 2 — bookmarks become the system clip (do first: core behavior depends on it)

- **Model**: `ClipKind { User, Bookmarks }` (serde lowercase, default `User`) on the new `Clip` row — kind discriminator, never name-matching. The Bookmarks clip is private, unrenamable (`update` refuses renames), undeletable (delete path refuses; account deletion cascades — hook the existing account-delete path), `is_public` forced false.
- **Migration** (same batch, both directions idempotent): forward — open-time bounded job (precedent: the backfill pattern Batch Q uses for media) — per account, find-or-create the `(owner, Bookmarks)` clip, then insert memberships for legacy rows missing from the clip. Ordering preserved: mint clip-note ids with `Id::from_parts(original created_at_ms, seq)` so `GET /bookmarks` newest-first order is bit-identical (old cursor values invalidate once — documented, one-time). Verify by count-parity + order test + idempotent rerun; then delete the old table path (rows are fully derivable from memberships while clips are on). Reverse — see **Disable path** below.
- **Disable path (two-way): turning clips off must not strand user data.** On startup with `clips_enabled == false` and clip rows present, run a bounded open-time job (same pattern as the forward migration):
  1. **Export**: per user with any clips, write `{data_dir}/clips-exports/{account_id}-{now_ms}.json` containing every clip (incl. the Bookmarks folder) with full membership lists (status ids + URIs), favorites, and clip metadata — so nothing curated is silently dropped. No new config key (reuses `data_dir`); document the location in `configuration.md`.
  2. **Bookmarks restoration**: re-derive legacy bookmark rows from the Bookmarks system-clip memberships (exact reverse of the forward mapping, order-preserving via membership `created_at_ms`; skip rows that already exist — idempotent). Bookmarks keep working exactly as before clips ever existed.
  3. **User clips go dormant, not deleted**: rows stay in place but are never served (routes 404) — re-enabling clips restores them byte-identical, and the export file is the belt-and-braces copy. Rationale: deleting on disable would make a flag flip destructive; dormancy keeps the toggle safe both ways.
  4. **Marker**: record `clips_disabled_export_at_ms` under the existing `META` table; re-export only when a clip row is newer than the marker, so repeated boots with clips off are no-ops. Round-trips converge: bookmarks added while clips are off are picked up by the forward migration on re-enable (it already inserts "legacy rows missing from the clip").
- **Trait stability**: keep `bookmark_status` / `unbookmark_status` / `bookmarked_statuses` signatures; reimplement them over `ClipStore` in the application layer so `interactions.rs`, `web.rs` (`web_cases::bookmarked`), status packing (`bookmarked` attribute), and all callers are untouched. `bookmarked_statuses` returns the existing `Bookmark` rows mapped 1:1 from clip memberships (cursor semantics unchanged).
- **MastoAPI surface: byte-identical behavior.** Same endpoints, same idempotency (double bookmark = same status), same pagination params (`max_id`/`since_id`/`min_id`/`limit`), same `bookmarked` attribute. The existing bookmark tests are the regression net — they must pass unmodified (plus new migration tests).
- **Module interplay**: bookmarks endpoints and the `Bookmark` port stay core (always on). The clips *surface* (routes, advertisement, non-bookmark ops) is what the flag gates. When clips are on, the Bookmarks folder shows in `clips/list|show` with a `system: true` marker (it *is* the user's clip); rename/delete of it 422s.

## Phase 3 — clips module (MastoAPI-convention REST, not a Misskey shim)

Deliberate divergence from pinetree: birchtree speaks MastoAPI conventions, so the surface mirrors `lists/*` shapes, not `clips/*` RPC params. (A Misskey-compat `/api/clips/*` shim is out of scope — documented.)

- **Shape** (per §Phase 1 domain shapes, federation columns included): `name` 1–100, `description` ≤2048 nullable, `is_public` default false. Pack: Mastodon-flavored entity (`id` decimal string, `title` — List-convention naming, not pinetree's `name` — plus `description`, `is_public`, counts where cheap). Record the naming choice; cross-impl familiarity loses to local convention here.
- **Endpoints**: `GET|POST /api/v1/clips`, `GET|PUT|DELETE /api/v1/clips/:id` (PUT for update, mirroring lists); members `GET /api/v1/clips/:id/statuses` + `POST` with `{status_ids: []}` + `DELETE` with same body (mirrors list push `account_ids[]`); favorites `POST|DELETE /api/v1/clips/:id/favourite` (+ British spelling, Mastodon convention). **No favourites-list endpoint in v1** (nothing consumes it; the clip pack carries counts + `is_favorited`). Favorites themselves stay (cheap parity, two endpoints).
- **Rules**: only the owner ever sees a private clip (404 to others, mirroring pinetree's rule); adding any existing visible status id is allowed (no visibility re-check, pinetree parity); dup pair → 422; per-owner and per-clip caps (mirror pinetree numbers: 10 clips/owner, 200 notes/clip — same constants, documented as shared policy).
- **Remote serving** (fed by Phase 6's fetch gate): `OrderedCollection` at the canonical URI + `?page=true` paging with the pinetree envelope (`totalItems`, `first`, `orderedItems` of **note URIs only**, never embedded notes); per-item gating reuses the existing status-visibility checks at serve time (a non-visible note resolves to nothing — same guarantee as pinetree). Collection carries an `updated` timestamp (cheap change detection for pollers).
- **Membership + metadata sync**: `Add`/`Remove` to the note author's inbox (owner-signed; authority = collection owner, verified by fetching the collection on miss and comparing `attributedTo`, exactly like pinetree — keep the rule identical so one matrix covers both); `Update(Collection)` with the embedded full document on rename/description/tier/flag changes; `Delete(Collection)` on delete **and** on tier flips to a more restrictive tier. All idempotent; activity-id deduped (existing `ActivityDedup` port). Tombstones: serve `410 Tombstone` (`formerType: OrderedCollection`) at deleted clip URIs (new `CLIP_TOMBSTONES` table — do NOT reuse the remote-tombstone table, whose ingest-gate semantics differ; mirror the `STATUS_BY_URI`-tombstone serving pattern instead).
- **Fan-out recipients (no new subscribe primitive):** note authors (direct, mirrors pinetree) **plus the clip owner's followers** (owner-follower fan-out — this *is* the follower-wide push, delivered over the existing follow graph + delivery infra with per-inbox jobs). Fan-out fires only for `Listed` + per-clip flag + module switch (aligns with pages; unlisted stays capability-pull only).
- **Inbound**: Add/Remove for remote collections ⇒ verify owner ⇒ evict-if-cached (birchtree v1 keeps no remote-clip store and no pull surface, so this is accept-and-evict only); `Update`/`Delete` for local clips from anyone but the (local) owner ⇒ ignored; unknown types ⇒ ignored per standard handling.
- **Notifications (user-facing half of "notify when a clip has been updated")**: new `ClipUpdated` kind (wire `clipUpdated`, pinetree-parity naming where applicable) + nullable `clip_id` on `Notification` (mirrors `fileId`/`draftId`); emit via the existing durable `Notify` path on: note added/removed (→ note author), metadata updated (→ followers), clip deleted/restricted (→ followers + member-note authors). Register the wire type wherever notification types are enumerated (receive-config validation, include/exclude filters).
- **Mounted only when `clips_enabled`.** Off ⇒ routes absent (404). Bookmarks endpoints unaffected (core, §Phase 2).

## Phase 4 — antennas module

- **Semantics**: port `~/pinetree/src/domain/antenna.rs` verbatim (keyword groups, exclude groups, src set, users/list sources, case-sensitivity, localOnly, excludeBots, withReplies, withFile) with the `Id` swap — subagent diff in Phase 0 is the spec.
- **Explicitly not Mastodon filters**: opposite semantics (collect vs hide); filters stay untouched.
- **Surface**: `GET|POST /api/v1/antennas`, `GET|PUT|DELETE /api/v1/antennas/:id`, plus `GET /api/v1/antennas/:id/statuses` — a **pull** matched-statuses timeline mirroring list-timeline conventions (cursor paging, viewer-aware visibility filtering). No streaming channel, no push in v1 (noted follow-up).
- **Engine placement**: matching runs at read time in the application op (same pull discipline as list timelines). If profiling later demands it, a match-sweep worker is a follow-up — do not build fan-out in v1.
- **Mounted only when `antennas_enabled`.**

## Phase 5 — pages module (+ visibility, gated ingest, Article fidelity)

- **Shape**: port pinetree `Page` **plus** the federation columns (tier, federate default ON, approved_actor_uris, uri): `title` (req, ≤256), per-user-unique `name` slug, `summary?`, `content` block array, `variables`, **`script`** (stored, never executed server-side, never served to MastoAPI clients raw), `eyeCatchingImageId?` (must be own file — same rule as upstream), `font`/`alignCenter`/`hideTitleWhenPinned`, `visibility` + `visible_user_ids` (new — see below), `likedCount`.
- **Read gating (prerequisite for everything below; pages are world-readable upstream, so any gate is a documented DIVERGENCE, narrowly scoped)**: `pages/show` (id + name+username forms), AP `GET /pages/:id`, `pages/like`, `pages/featured` (public-only) all enforce `visibility`: `Public`/`Unlisted` ⇒ anyone; `Private` ⇒ owner or follower-of-owner (local follow edge — checkable without new infra); `Direct` ⇒ owner or listed id. Misses *and* denials are the same 404 shape (existence unconfirmed). Moderators bypass (consistent with `pages/delete`). Old rows decode `Public`; local creates are always `Public` (no UI change, upstream parity holds for local content).
- **Gated articles become pages (owner decision — no downgrade to notes).** Ingest mapping per the Phase 1 table; malformed objects (not an object, `type != "Article"`, missing/empty `id`) keep the old note path as a parse-failure fallback. Non-`Public` rows keep `script: ""`, eye-catch from local files only. **No retro-conversion**: existing Article-derived notes stay notes.
- **Origin fidelity**: store the Article's `published`/`updated` on ingest (fall back to row times when absent); serve those, not ingest time. Re-rendered `content` stays sanitized-HTML-from-blocks; `attachment` re-emits eye-catch (local file → URL) plus whitelisted `http(s)` remote attachment URLs verbatim (never fetched inline).
- **`source` extension, documented in `docs/federation-contract.md`**: `{content, variables, presentation: {font, alignCenter, hideTitleWhenPinned}, name}` — carries everything `Article` can't say natively, readable only by pinetree-class peers; everyone else uses `content`. No new top-level fields, no new object type (revisit criteria in §"New object type" below hold).
- **`to`/`cc` audit**: Listed ⇒ `[Public]` + followers-cc (existing behavior); Unlisted ⇒ empty (capability-only, no public claim); Followers-audience ⇒ followers-cc only; Direct ⇒ addressed actors only. The gate enforces; the claim must not overstate.
- **Surface**: `GET|POST /api/v1/pages`, `GET|PUT|DELETE /api/v1/pages/:id`, show-by-`(username, name)` lookup, `like`/`unlike`, `featured` (top-10 by likes — port pinetree's upstream finding, no admin curation). Pack: blocks raw + pre-rendered sanitized HTML (`content_html`) so MastoAPI-shaped clients can render without understanding blocks; script never packed.
- **Outbound**: `Create`/`Update(Article)` fan-out to owner followers on create/edit (listed tiers only; embed the full document like pinetree); `Delete(Article)` on page delete; `Like` with the page-URI object on likes of materialized remote pages (delivered to the origin's inbox); page likes also notify the page owner (new `PageLiked` kind, wire `pageLiked`, nullable `page_id` on `Notification`).
- **Mounted only when `pages_enabled`.**

## Phase 6 — federation wiring + interop proof (new: federation is v1, not deferred)

Everything above builds the *shapes*; this phase makes them *move* between birchtree and pinetree. No new object types anywhere in this phase (see §"No new object type" below) — only serving, signing, delivery, and verification of the stock AP vocabulary both sides already speak.

- **Fetch gate** (port pinetree's `fetch_gate.rs` discipline onto birchtree's HTTP client + key lookup + actor resolution): verify HTTP Signatures on inbound GETs for the three collection/article routes, resolve the signing key to an actor URI (local cache, fetch on miss, one refresh on rotation), then `decide_fetch` mapping — `Serve`→serve, `Forbidden`+identified→403, `Forbidden`+anonymous→404, `Hidden`→404. Pure function tests for the matrix (tier/visibility × signer), no network.
- **Clip serving + sync**: `GET /clips/{id}[?page=true]` and `GET /users/{id}/clips` through the gate with per-item visibility filtering; `updated` on the collection; Tombstone serving for deleted clips; Add/Remove fan-out (note authors + owner followers, listed tiers only) and inbound verification (fetch + compare `attributedTo`, same rule as pinetree — one matrix covers both); `Update(Collection)` with embedded full doc; `Delete(Collection)` + tombstone write on delete and on tier flips to more restrictive.
- **Page serving + sync**: `GET /pages/{id}` through the gate; `Create`/`Update(Article)` fan-out (listed tiers only, full-document embed); `Delete(Article)` on delete (new: pinetree emits none today — birchtree sends them; pinetree already accepts them, and its TTL revalidation covers the gap the other way); inbound `Like(page)` counting; inbound `Update(Article)` refresh by canonical URI; inbound `Delete(Article-URI)` retract with same-host-or-owner authority check.
- **No inbound clip-follow handling in v1.** A remote `Follow(Collection)` hits the unknown-activity path (ignored, logged at debug). Rationale: follower-wide push is already delivered via owner-follower fan-out (existing graph, no new primitive), so nothing functional depends on it; accepting follows would need the full subscribe UX to be meaningful. Revisit pointer recorded below.
- **Pinetree gaps, filed not fixed**: pinetree (as of this writing) emits no `updated` on collections, no outbound `Delete` for clips/pages, and no Tombstone serving — birchtree tolerates all three (TTL revalidation; treat post-known-ID 404 as "refetch later, keep cache" rather than proof of deletion — never delete cached state on an unsigned 404). These are recorded here as upstream follow-ups owned elsewhere, not blockers: interop converges through TTL even where push is one-sided.

### No new object type (decision record)

`Article` + `OrderedCollection` + the documented `source` extension carry everything both implementations need. A custom type would be dropped or degraded by every other server, forcing a dual representation (custom + `Article` fallback) for zero current consumers. Revisit **only if**: a second implementation wants block-level interop (machine-readable page structure across servers) that `source` cannot express. Until then the `source` schema *is* the extension point.

### Interop proof (the v1 federation gate)

Against a live pinetree checkout (same pattern as this repo's `federation_interop` tests; pinetree runs its own nodes, birchtree runs its own — no shared runtime):

1. Clip resolve both directions with per-item gating (incl. a private note correctly absent, unlisted by capability, remote-approved 403/201-style matrix, unknown/unsigned 404s).
2. Add → remote inbox → membership converges; Remove likewise; metadata edit → `Update` converges; delete → `Delete` + Tombstone converge; re-fetch after TTL converges without push.
3. Article round-trip both directions (title/summary/content/eye-catch, `Update` converges, script-bytes grep on every federated byte, both directions), page-like round-trip both directions with replay convergence.
4. Gated article matrix: followers-only and specified `Article`s materialize as non-public pages, invisible to strangers on *every* surface (show/AP/like/featured/export + pack), with zero script bytes anywhere; no note rows created for them.
5. Switches off on either side ⇒ zero footprint (routes 404, no advertisement, no resolution, no deliveries enqueued).

## Phase 7 — advertisement, contracts, gates

- **Instance advertisement**: extend the existing instance DTO builders (`dto::instance_v1_json` / `instance_v2_json`) with a namespaced extension object, e.g. `"birchtree": {"clips": true, "antennas": false, "pages": false}` — ignored by Mastodon clients, machine-readable for custom ones. Document as extension.
- **Contracts** (AGENTS.md state maintenance): endpoint-manifest entries per new endpoint (method/path/scopes/params/response/4xx/404-when-off) backed by contract tests; `domain-contracts.md` shapes; `application-ports.md` new ports; `persistence-conformance.md` note; `wiki/` + README can/can't lists.
- **Sweep/scripts**: extend smoke coverage where new endpoints un-404 (behind flags on); note which suites run with flags on vs off.
- **Master rules check**: no success-shaped stubs (disabled = 404), one txn per mutation + jobs, inputs validated at the edge, decimal-id ordering everywhere.

## Gates (exit deliverables)

1. `cargo check --all-targets` + `cargo test` green (AGENTS.md verification rule; counts recorded, incl. new conformance tests).
2. `bash scripts/smoke-all.sh` → pass (the AGENTS.md full gate).
3. Module-off matrix: every new route 404s with flags off; bookmarks + all core endpoints unaffected (regression suite green both ways).
4. Migration proof: legacy bookmark rows → system clip with count + order parity, idempotent rerun, old path deleted.
5. Interop proof (§Phase 6 matrix, both directions against live pinetree): clip resolve + membership/metadata/delete convergence, Article round-trip + gated-article matrix, like round-trip, switches-off zero footprint.
6. State logs per AGENTS.md with executed evidence (paste or reference real output).

## Non-goals (explicit)

- Antenna federation of any kind (no peer implementation exists; pinetree antennas are local-only too — no wire to agree on).
- Inbound clip-follow handling (`Follow(Collection)` ignored in v1; revisit pointer in Phase 6 — follower-wide push already works via owner-follower fan-out).
- Streaming channels for antenna matches.
- Misskey-compat `/api/clips|pages/*` shim (deliberate divergence; MastoAPI conventions instead).
- `env:` indirection for the new flags (no secrets involved).
- Cargo-feature compile stripping (single binary stands).
- Custom roles, OAuth changes beyond new scopes, mail, anything in Batch Q's punch list (reconciled in Phase 0, owned by Q).

## Owner decisions (all three resolved — build as specified)

1. **Bookmarks folder visibility when clips are on**: DECIDED — visible in the clips list with `system: true` (rename/delete 422). A system folder users can see but not rename/delete is the least surprising.
2. **Migration strategy**: DECIDED — two-way. Forward (clips on): one-way migration into the system clip, old table path deleted. Reverse (clips off): per-user export to `{data_dir}/clips-exports/` + bookmarks re-derived from the system clip + user clips dormant in place. Both directions idempotent; round-trips converge (§Phase 2).
3. **Antenna endpoint namespace**: DECIDED — `/api/v1/antennas`.
