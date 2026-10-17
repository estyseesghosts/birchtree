# 2026-10-17 — Scope expansion: admin/moderation, push, transcoding, search engine, remote threads

Phase reference: `birchtree.md` §1, §4.2, §5, §6.3, §6.4, §7 (Phase 6, Phase 7, new Phases 7.1–7.3, Phase 8), §8, §9, §10. Documentation only. No changes to `src/`, `tests/`, `scripts/`, `Cargo.toml`, `AGENTS.md`, any `docs/phase-*-exit-criteria.md`, `docs/logs/`, or the Phase 4 state logs.

## Goal

Apply four operator scope decisions to `birchtree.md` and the contracts it drives, and make the documentation coherent:

1. Admin API, moderation (reports), Web Push, conversations, suggestions, announcements, trends, annual reports and donation campaigns are in profile, each with a planned phase. The server-rendered admin dashboard and the Mastodon web app stay out.
2. Media transcoding is a planned milestone (animated GIF → WebM, a deliberate deviation from Mastodon's GIF → MP4 `gifv`). The no-ffmpeg behavior is interim.
3. Search must be backed by a real engine. The engine choice is open. The local-corpus search is interim.
4. Remote thread completion belongs in Phase 6. Unresolved remote parents are temporary, and the mechanism is recorded against the pinned tag.

Operator clarification applied: birchtree is backend-only. keystodon is a separate frontend developed in parallel, is not on this machine, and is the planned interop target.

## Precondition check (executed)

- `git status --short` at start: clean. HEAD: `ea16ceb feat: Phase 4 — timelines + phase exit`.
- `state/logs/2026-10-17-phase-4-timelines-exit.md` present; `docs/phase-4-exit-criteria.md` present.
- Reference checkout `~/fedifedifedi/mastodon` is on `main`. Tag `v4.7.3` resolves. All pinned claims below were read with `git show v4.7.3:<path>` or `git grep … v4.7.3`.

## Files changed

| File | Change |
|---|---|
| `birchtree.md` | §1 keystodon/backend-only and declared-profile bullet; §4.2 unknown-parent rule extended (not deleted); §5 “Media transcoding (Phase 7.1)”; §6.3 admin scope and role bullet; §6.4 media packer note; §7 Phase 6 remote-thread, signed-fetch and crawl bullets; Phase 7 rewritten (client-facing groups); new Phases 7.1, 7.2, 7.3; Phase 6 and Phase 8 exits updated; §3 tree (transcode, search, admin, endpoint files); §8 item 9; §9 non-goals and open decisions; §10 pinned sources |
| `docs/federation-contract.md` (Draft) | Initial activity set (+Flag); “Signed fetch and authorized fetch”; new “Remote thread completion (Phase 6)” with upstream mechanism table, corrections, birchtree contract A–F, and limits |
| `docs/compatibility-profile.md` (Draft) | Media and search interim notes; new “Planned in-profile surface”; Exclusions rewritten; Open decisions updated |
| `docs/endpoint-manifest.md` (Draft) | Profile legend; admin-role note; 99 new rows (sections below); 2 rows reclassified; 3 discovery rows Plan → In; notes on context, `/api/v2/search`, and the three domain-block rows; bullet section relabeled and pruned; media deviation reframed (interim, WebM deviation recorded); new “Search deviations”; new “Route inventory discrepancies and gaps” |
| `docs/README.md` | Added the missing `phase-4-exit-criteria.md` index row (coherence fix, outside the handoff list) |
| `docs/wiki/Roadmap.md` | Phase table with 7.1–7.3; keystodon note; non-goals reworded |
| `docs/wiki/Feature-Matrix.md` | New `planned` rows for every re-scoped surface; transcoding and search rows updated |
| `docs/wiki/Current-State.md` | Known gaps and not-yet-implemented lists updated for the new phases |
| `docs/wiki/About.md` | Backend-only and keystodon paragraph; stale “Phase 2” sentence corrected to Phases 2–4 |
| `docs/wiki/Home.md` | Status line corrected (Phases 0–4 implemented; Phase 5 next); related-documents pointer corrected to the Phase 4 gate |
| `state/logs/2026-10-17-scope-expansion.md` | This entry (new) |

## Phase assignment (proposed)

| Phase | Scope added or changed | Why here |
|---|---|---|
| 5 | Unchanged | — |
| 6 | Remote thread completion; Flag send and receive; instance-actor signed fetch; authorized-fetch endpoint signing | Thread completion is federation; Flag is required by reports; reports forwarding and server-level domain blocks need federation |
| 7 | Conversations (4 routes + 2 mute routes); suggestions v1/v2 (4 routes; the v2 pair was omitted by the handoff); announcements (5); donation campaigns (1); Web Push (5); `/api/web/*` (7) | Client-facing, no role model needed, no federation dependency (remote DMs wait for Phase 6) |
| 7.1 | Media transcoding (animated GIF → WebM; video and image as sub-decisions) | Changes wire types and the media lifecycle, so it sits with the other parity work. No dependency on Phases 5 or 6, so it may be scheduled earlier |
| 7.2 | Search engine behind `GET /api/v2/search` | Engine choice open; index is derived state. No dependency on Phases 5 or 6 |
| 7.3 | Admin API (63 rows), reports (1), trends (4 public), annual reports (5), admin metrics (within the admin rows) | Needs a role model, approval modes, suspension state and Phase 6 enforcement points. Placed before the Phase 8 performance gate so the gate covers it |
| 8 | Unchanged name and scope; exit now covers Phases 0–7.3 surfaces | Final performance gate |

Naming choice: sub-phases 7.1–7.3 rather than renumbering Phase 8 or appending Phase 9. Renumbering would churn references in the roadmap and the matrix. Appending after Phase 8 would put feature work after the performance gate.

## Source facts verified at v4.7.3 (summary)

Route inventory (`config/routes/api.rb`):
- Admin API is nested under `namespace :v1` (`/api/v1/admin/*`) and `namespace :v2` (`/api/v2/admin/accounts`). It is not a top-level `admin` namespace.
- Push routes are under `namespace :v1 → :push` (`/api/v1/push/subscription`). **There is no `config/routes/push.rb`** (the handoff named one).
- `config/routes/admin.rb` is drawn for the server-rendered admin dashboard, which is a web UI route, not the API.
- Web routes are under `namespace :api → :web` (`/api/web/*`).
- Trends: `resources :trends … controller: 'trends/tags'` gives `/api/v1/trends`; `namespace :trends` gives `/tags`, `/links`, `/statuses`. **`/api/v1/trends/links` was missing from the handoff.**
- `v2` has `suggestions` (`GET /api/v2/suggestions`, `DELETE /api/v2/suggestions/:id`). **Missing from the handoff.**
- `resource :domain_blocks, only: [:show, :create, :destroy]` at the top of `v1` (singular). See discrepancies.

Auth and scopes:
- Admin controllers: `admin:read` / `admin:write` (and `admin:read:<resource>` / `admin:write:<resource>`) through `authorize_if_got_token!`; all include `Admin::PermissionsConcern`, which enforces `Admin::BasePolicy#access?` before the scope check.
- Doorkeeper `optional_scopes` in `config/initializers/doorkeeper.rb` registers all `admin:*` scopes, `push`, `write:reports`, `write:conversations`, `write:mutes`, and others.
- `Api::BaseController#current_user` = `current_resource_owner || super` (a Devise session is accepted on API routes too).
- `Api::Web::BaseController`: `protect_from_forgery with: :exception` (CSRF). `web/settings`, `web/push_subscriptions`: `require_user!`.
- `announcements` index: `require_user!` only. `donation_campaigns`: `require_user!` only.
- v1 suggestions: `deprecate_api '2021-05-16'` (`Deprecation` + `Sunset` headers). v1 trends `/api/v1/trends` only: `deprecate_api '2022-03-30'`.
- Push subscription model: `belongs_to :access_token`.

Remote threads:
- Ingest: `Create#resolve_thread` → `ThreadResolveWorker` (queue `pull`, retry 3). Inline replies: `Create#fetch_replies` → `FetchRepliesService` (`allow_synchronous_requests: false`, `MAX_REPLIES = 5`, same-host filter) → `FetchRepliesWorker` otherwise.
- Read: `Api::V1::Statuses::ContextsController#show`. Authenticated viewer + `should_fetch_replies?` → `Mastodon-Async-Refresh: id="…", retry=3[, result_count=N]` + `ActivityPub::FetchAllRepliesWorker` (batch). Anonymous viewers never fetch. Context limits: 4096 authenticated; 40 / 60 / depth 20 anonymous.
- Crawl: `FetchAllRepliesWorker` (`MAX_REPLIES = 1000`, `MAX_PAGES = 500`) → `FetchAllRepliesService` (`MAX_REPLIES = 500`) → `FetchReplyWorker`.
- Cooldown constants: `FETCH_REPLIES_INITIAL_WAIT_MINUTES = 5`, `FETCH_REPLIES_COOLDOWN_MINUTES = 15`.
- `FetchRemoteStatusService`: `DISCOVERIES_PER_REQUEST = 1000` per `request_id`, 5-minute TTL; `domain_not_allowed?`; `trustworthy_attribution?`; suspended actors return nil.
- Signing default: `JsonLdHelper#fetch_resource_without_id_validation` sets `on_behalf_of ||= Account.representative`.
- Authorized fetch: `AuthorizedFetchHelper#authorized_fetch_mode?` (`AUTHORIZED_FETCH` or `Setting.authorized_fetch`, or limited federation mode). `require_account_signature!` on replies, contexts, collections, likes, shares, outboxes, quote authorizations, featured collections, feature authorizations.
- Feed withholding: `FeedManager#filter_from_home` returns `:filter` for a reply with nil `in_reply_to_id` or nil `in_reply_to_account_id`.

Media:
- `MediaAttachment.file_processors`: `image/gif` → `[:gif_transcoder, :blurhash_transcoder]`; video → `[:transcoder, …]`; other images → `[:lazy_thumbnail, …]`.
- `lib/paperclip/gif_transcoder.rb` converts to `.mp4`, `video/mp4`, `type = gifv`. The enum is `image: 0, gifv: 1, video: 2, unknown: 3, audio: 4`.
- ffmpeg is configured through `config/initializers/ffmpeg.rb` (`FFMPEG_BINARY`).

Search:
- `SearchService#status_searchable?` = `Chewy.enabled? && status_search? && @account.present?`. Upstream returns **no status hits without Elasticsearch**. Account search is not gated the same way.

Other:
- Web Push uses VAPID (instance configuration exposes `vapid`).
- Donation campaigns: returns `204` when `Rails.configuration.x.donation_campaigns.api_url` is blank or no campaign is returned; 1-hour cache; stoplight (threshold 10, cool-off 60 s).
- Reactions: `PUT`/`DELETE /api/v1/announcements/:announcement_id/reactions/:id`, with `:id` as the reaction name.

## Contradictions with the handoff (surfaced, not papered over)

1. **Decision 1, “existing operator controls”.** The admin API requires a moderator-or-admin role (`Admin::PermissionsConcern`). Birchtree has no role model (`src/` has only a fixed `roles: []`/`role` DTO value). The CLI creates accounts without roles. Phase 7.3 therefore adds a role model and CLI role grants before any admin route. This is a new prerequisite, not an existing control.
2. **Decision 1, route paths.** The handoff named `config/routes/push.rb` and `config/routes/admin.rb` as the API sources. Push routes are in `api.rb`. `admin.rb` is the web dashboard. The admin API paths are `/api/v1/admin/*` and `/api/v2/admin/accounts`. The docs use the correct paths.
3. **Decision 1, omissions.** `/api/v1/trends/links` and `/api/v2/suggestions` were not in the handoff. Both are added as rows, in their groups.
4. **Decision 1, `web/*` auth.** The handoff treats `web/*` as in-profile API. Upstream guards it with the Devise session and CSRF, not OAuth bearer tokens. Whether a bearer-token client, including keystodon, can use these routes is untested. This is an open decision, recorded in the profile and the manifest.
5. **Decision 4, read-time ancestors.** The handoff asks for “ancestor dereferencing on read”. Upstream does not do that. Ancestors resolve at ingest through chained `ThreadResolveWorker` jobs, one hop each, with no depth cap. The read path completes descendants only, and only for authenticated viewers. Read-time ancestor completion is kept as an explicit, off-by-default birchtree extension, which needs a recorded deviation before it can ship.
6. **Decision 4, signed fetch.** Thread fetches are signed by default with the instance actor (`Account.representative`), not sent unsigned. The Phase 6 requirement is therefore an instance-actor keypair.
7. **Decision 4, same-host rule.** The inline reply path has a same-host filter. The all-replies crawl overrides `filter_replies` without calling `super`, so it skips that filter. Birchtree's contract applies the rule to the crawl too, as a recorded deviation.
8. **Decision 3, premise.** The handoff framed search as “not necessarily Elasticsearch”. The pinned source shows upstream status search is hard-gated on Elasticsearch (`Chewy.enabled?`). Birchtree's interim status scan therefore returns hits upstream would not. This supports Decision 3 and is now recorded as a deviation.
9. **Decision 2, premise.** Confirmed: upstream produces `gifv` with an MP4 rendition. The WebM deviation is a birchtree choice, recorded as such.

## Discrepancies found in existing docs or code (not fixed here)

1. **Domain-block route shape (Phase 3 rows; implemented).** The pinned tag declares `resource :domain_blocks, only: [:show, :create, :destroy]`: `GET`, `POST` and `DELETE` on `/api/v1/domain_blocks`. Birchtree implements `GET`/`PUT /api/v1/domain_blocks` and `DELETE /api/v1/domain_blocks/{domain}` (`src/api/mastodon/router.rs`, lines 134–140; handlers in `src/api/mastodon/endpoints/relationships.rs`). This needs a Phase 3 contract review and a code change. It is recorded in the manifest and not fixed here.
2. **`GET /api/v1/accounts` (index) has no manifest row.** The pinned `resources :accounts, only: [:index, :create, :show]` declares it. Birchtree has `POST` and `GET /:id` only.
3. **`KNOWN_SCOPES` vs. its comment.** `src/api/oauth/scopes.rs` says admin scopes are "accepted for storage but never granted". `KNOWN_SCOPES` lists no `admin:*` scope, so they are rejected. Phase 7.3 must reconcile this.
4. **Pinned routes not yet rowed** (outside this re-scope): `profile`; `directory`; `instance` sub-resources and `peers/search`; `emails/confirmations`; `domain_blocks/preview`; `tags` and `followed_tags`; `featured_tags`; account sub-resources (`lists`, `identity_proofs`, `familiar_followers`, `email_subscriptions`, `collections`, `in_collections`); top-level `collections`; notification requests and policy; `GET /api/v2/notifications` and its sub-routes. Listed in `docs/endpoint-manifest.md`.
5. **`GET /api/v1/custom_emojis` and the relation collections** (`blocks`, `mutes`, `favourites`, `bookmarks`) exist only as bullets, not rows. Unchanged in this re-scope; listed as a gap.
6. **`docs/domain-contracts.md` line 16** (“withheld from ordinary reads until linked”) is still consistent with §4.2. It does not mention active resolution. Outside this handoff's file scope; recommend extending it.
7. **`docs/source-audit-v4.7.3.md`** has no “Differences” entries yet. The verified differences above should be recorded there. Outside this handoff's file scope.
8. **`docs/visibility-and-authorization.md`, `docs/architecture.md`, `docs/application-ports.md`** do not yet cover admin scopes, the transcode port, or the search port. Outside file scope; needed when 7.1–7.3 are scoped.
9. **`ACTIVITYPUB-FEDERATION-RUST.md`** (repo root) already names admin/moderation as separately testable modules and lists reports as `Flag`. Consistent with this re-scope; not edited.

## Route rows (`docs/endpoint-manifest.md`)

- Route rows at HEAD: **117**. Route rows now: **216**. Net added: **99** (verified with `grep -c` on the table rows).
- Added by group: admin v1 **62** (accounts 10, reports 8, domain allows 4, domain blocks 6, email-domain blocks 4, IP blocks 6, trends approval 12, metrics 3, canonical email blocks 5, tags 4); admin v2 **1**; reports **1**; conversations **4**; suggestions **4** (v1 2, v2 2); announcements **5**; annual reports **5**; donation campaigns **1**; push **5**; `/api/web/*` **7**; public trends **4** (`/api/v1/trends`, `/tags`, `/links`, `/statuses`).
- Reclassified Out → In: **2** (`POST /api/v1/statuses/:id/mute|unmute`, now `planned (Phase 7)`).
- Profile changed Plan → In (no `Plan` rows remain): **3** (`GET /.well-known/webfinger`, `GET /.well-known/nodeinfo`, `GET /nodeinfo/2.0`; all `planned (Phase 6)`). Flagged here because the handoff did not ask for it. It makes the Profile column consistent.
- Notes-only changes: context row (Phase 6 `Mastodon-Async-Refresh`); `GET /api/v2/search` (interim note); the three domain-block rows (route-shape discrepancy).
- Bullets removed from “Explicitly excluded”, now rows: `annual_reports`, `announcements`, `conversations`, `donation_campaigns`, `suggestions`, `POST /api/v1/reports`, `namespace :admin`, `v2` admin, `push/*`, `web/*`. Kept as bullets (not rowed): `oembed` (Out), `v1_alpha` (Out), `custom_emojis` (implemented, listed as a gap), relation collections, Phase 7 collections and accounts sub-resources, `translate` (Out).

## Verification (executed)

- Stale-phrase scan over the edited files (`grep -n -i -E "out of scope|excluded|out of profile|pending doc|open decision|full administrative|admin dashboard|search-engine|no ffmpeg|no transcod|Plan\b"`): every remaining hit is intentional (new “out of profile” statements, interim deviation notes, the open-decision list).
- Manifest row counts (`grep -c`): all route rows 216; admin v1 62; admin v2 1; reports 1; conversations 4; suggestions 4; announcements 5; annual reports 5; donation campaigns 1; push 5; `/api/web/*` 7; public trends 4. HEAD: 117. Rows with `| Plan |` profile: 0.
- Changed-line check (`git diff HEAD -- docs/endpoint-manifest.md | grep -c '^+| '`): 109 = 99 new rows + 10 changed rows (2 mute, 3 discovery, 1 context, 1 search, 3 domain blocks).
- `git diff --stat` (10 files): `birchtree.md`, `docs/README.md`, `docs/compatibility-profile.md`, `docs/endpoint-manifest.md`, `docs/federation-contract.md`, `docs/wiki/{About,Current-State,Feature-Matrix,Home,Roadmap}.md`. New: this log.
- Status headers: `docs/federation-contract.md`, `docs/compatibility-profile.md`, `docs/endpoint-manifest.md` remain `Status: Draft`.
- Not run: `cargo check --all-targets`, `cargo test`, `scripts/smoke-all.sh`. No code, tests or scripts changed, and this change claims no phase exit.

## Decisions taken without asking (reversible)

- Sub-phase naming 7.1–7.3 (see Phase assignment).
- Admin/moderation in Phase 7.3, before the Phase 8 gate.
- Trends, annual reports and reports in Phase 7.3, since they need approval workflow or moderation queues. Public trends are approved-only upstream.
- Push and `/api/web/*` in Phase 7. `/api/web/*` marked “In (confirm)” pending keystodon's auth mode.
- Added `GET /api/v1/trends/links` and `GET`/`DELETE /api/v2/suggestions` rows, which the handoff omitted.
- Discovery rows Plan → In (see Route rows).
- Thread-completion caps (40 hops; 1000-fetch budget per trigger) are proposals, labeled as such, with upstream constants used where they exist.
- Same-host rule on the crawl recommended, recorded as a deviation.
- Read-time ancestor completion documented as off by default.
- Added the missing `phase-4-exit-criteria.md` index row to `docs/README.md`, and corrected stale wiki status lines (`Home.md`, `About.md`).

## Open questions (for the operator)

1. Confirm the phase placement (7.1–7.3, or renumber/append).
2. Will keystodon authenticate with OAuth bearer tokens or a browser session? This decides whether `/api/web/*` belongs in profile.
3. Adopt read-time ancestor completion? Upstream does not; default is no.
4. Role model: which roles and permissions, and how are they granted (CLI command shape)? Is an admin-action audit trail required?
5. WebM codec set; whether video and images are transcoded; whether `type: gifv` stays for WebM-backed GIFs; GIF upload `202`/`206` lifecycle.
6. Search engine: embedded library or external service?
7. Should the crawl apply the same-host rule (recommended) or match upstream (no)?
8. Domain-block route shape: fix Phase 3 to the pinned `show`/`create`/`destroy` shape? Which phase carries the code change?
9. `GET /api/v1/accounts` (index): in profile, and in which phase?

## Next step

Phase 5 (streaming) per `AGENTS.md`. Before Phase 7 sign-off, resolve discrepancy 1 (domain-block shape) and discrepancy 2 (missing `GET /api/v1/accounts` row). Record the verified differences in `docs/source-audit-v4.7.3.md` and extend `docs/domain-contracts.md` in a follow-up documentation pass (outside this handoff's file scope). Phase 0's route-inventory review remains open and now also covers the unrowed routes listed above.
