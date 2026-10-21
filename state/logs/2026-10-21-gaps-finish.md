# 2026-10-21 — gaps finish (`docs/gaps.md`) parent-led build

Parent ran Step 0, then G-A/G-B/G-C/G-D/G-E on disjoint file sets on
`master`. Shared files (`docs/*`, `ports.rs`, `redb/*`, `config.rs`/
`main.rs`, `router.rs`, `domain/job.rs`, `jobs/*`, conformance tests)
parent-merge only via snippets. Agents left changes uncommitted, never ran
commit/stash/reset/checkout. Parent merged everything below, ran the full
gate, and committed (explicit operator instruction for this batch).

`git status --short` at start: only `?? docs/gaps.md` (the task doc
itself). Step 0: schema v18 (`COLLECTIONS`, `COLLECTION_ITEMS`,
`TOTP_SECRETS`, `WEBAUTHN_CREDS`), `JobKind::{QuoteReverify,
CollectionFanout, RelayForward}` no-op arms, config keys
`password_reset_ttl_hours` (6), `site_terms/privacy/extended_file`, CLI
flags, `docs/config-schema.json`, round-trip test. `cargo check
--all-targets` EXIT=0; `cargo test --lib config::` 12/12.

## What changed

- G-A (operator/profile): reset page + set-password (`GET
  /auth/password/edit`, `POST /auth/password`, 6h consume-on-write
  bearers, indistinguishable 422); site texts from
  `--site-*-file` (absent keeps honest shapes); rule translations store
  (`RULE_TRANSLATIONS`, schema v19); 68-entry ISO-639 catalog;
  `GET/PATCH /api/v1/profile` + avatar/header DELETE; `identity_proofs=[]`;
  `create-text` + `set/delete-rule-translation` CLI. Suite
  `tests/gaps_profile_surfaces.rs` (13).
- G-B (notifications/streaming): all D51 producers wired in-txn (`status`,
  `update` + `quoted_update`, `moderation_warning`, `annual_report`,
  `admin.sign_up`, `admin.report`, `collection_update` poll hook;
  `severed_relationships`/`added_to_collection` builders real but unwired);
  `announcement`/`announcement.reaction`/`announcement.delete` on `user`
  streams; `include_filtered` served; `supported_types` fallback rendering;
  push `Unsubscribe-URL` backfill. Suite `tests/gaps_notifications.rs` (12).
- G-C (collections/REST): CRUD + items + revoke, `in_collections`,
  account `collections`/`lists`, `domain_blocks/preview`, `peers/search`,
  minimal `v1_alpha`, full `v2/notifications` family; 22 routes mounted;
  `CollectionStore` moved to `ports.rs` + `Store`; `CollectionFanout`
  runs `handle_fanout` in the sync drain. Suite
  `tests/gaps_collections.rs` (12).
- G-D (federation tail): Move + migrated follows + `moved_to` rendering,
  remote emoji ingest + status-array rendering (`REMOTE_EMOJI`, schema
  v20), relays (`RelayForward`, `relay_inboxes`, public-content-only
  fan-out) + inbox forwarding, counters-as-hints, Note `replies` +
  `featuredTags` (local-gated), `QuoteReverify` worker, poll
  `collection_update`, Flag forwarding, live WebFinger in `lookup`, embeds
  remote fetch+sanitize; D32 narrowed to truly-unknown (202). Suite
  `tests/gaps_federation_tail.rs` (19).
- G-E (auth/moderation): TOTP (+ hashed backup codes) + WebAuthn (ES256)
  with staged browser gate (`otp_attempt`); `time_zone`/`date_of_birth`
  capture + min-age gate; IP-block proxy matrix; email-block matrix;
  D35 per-action flags; numeric role ids + real bitmask (`-1` gone).
  Suite `tests/gaps_auth_moderation.rs` (23).
- Parent finish: `hide_collections` enforced (REST edges, federated
  pages, items, familiar followers — D52/D30 closed); `reblogged_by`
  distributable scope; counts-only `SyncFollowers` worker (new
  `JobKind::{FetchRemoteEmoji, SyncFollowers}`; lookup + Move triggers);
  `minimum_age_years` plumbed (default 16); durable `WEBAUTHN_CHALLENGES`
  (schema v21); targeted email-domain signup lookup (domains unique);
  `set_remote_stats` port. Decisions in `docs/gaps.md` (finish section):
  `severed_relationships` stays a gap (no sever cascade),
  `email_subscriptions` stays 404 (no pinned shape).

Parent deviations from proposed snippets (reviewed, all stricter or
equivalent): relay fan-out allowlisted to public content events (never
likes/flags/quotes/blocks); `featured_tags` local-gated; quoter scan
inlined on the held write txn (`quotes_of_status` cannot nest);
`RemoteEmojiRow.url` is the `system/` asset path (renders never hotlink);
`role_json` numeric ids; moderator sum corrected to `1049884` (verified
against `user_role.rb` at `8ff11c2b`; old `1049888` was arithmetic slip).

Schema ledger: v18 collections + second-factor creds, v19 rule
translations, v20 `REMOTE_EMOJI`, v21 `WEBAUTHN_CHALLENGES` (all tables +
migrations, no backfills). Docs updated in place: 21-row manifest
section + D12/D30/D32-family/D35/D37/D51/D52 rows + TODOs sections.

## Verified (real output)

- `cargo check --all-targets` → EXIT=0 (only pre-existing warnings:
  `transcode.rs:731 unused pid`, `remote_cache.rs full_entry dead code`).
- Five gaps suites → 79/79 (`auth_moderation` 23, `federation_tail` 19,
  `profile_surfaces` 13, `notifications` 12, `collections` 12), EXIT=0.
- `cargo test` (full) → EXIT=0, 69 suites `ok`, 0 failed. Two
  parent-fixed regressions on the way: `config_file`
  schema-parity (added `minimum_age_years` to `config-schema.json`, 6/6)
  and `phase7_3_admin_accounts_reports` old slug-id assertions (now
  numeric 0/1, 8/8).
- `bash scripts/smoke-all.sh` → `smoke-all: green` (check + full test +
  phase1–7.3 all `SMOKE_OK`).
- Live lab (`gaps-lab.sh`, test-profile binary, port 38999) → `gaps-lab:
  green`: boot; reset form 200 + bogus-token 422; site texts served;
  `Français` catalog; profile read/patch/avatar-delete; `identity_proofs`
  `[]`; collections CRUD (private hidden from Bob 404), items add/index,
  `in_collections`, revoke, `v1_alpha`; hide gate (empty members, counts
  stay, followers hidden); v2 index/unread/policy/dismiss-404; preview;
  peers-search; account lists.
- Move/relay/emoji-federation evidence is in-suite (loopback lab runs in
  `gaps_federation_tail`, 19/19). AP-level collection Add/Remove
  activities are NOT sent/received (out-of-profile kinds stay
  accepted-and-dropped under D32-narrow); fan-out notifies local member
  authors only.

## Open (not regressions)

- MFA enrollment has no REST surface (use cases + stores + browser
  verify-gate exist; no enroll/verify endpoints). Next batch wires it.
- TOTP envelope encryption at rest (no operator key); RS256/EdDSA +
  attestation chains (422); account-display-name remote emoji (status
  arrays done); moderator `require_admin_action` adoption beyond accounts;
  `severed_relationships` trigger needs a sever cascade first.
- Parity suite (`birchtree.md` §8 item 11) still not run (no harness).

## Environment notes (not code)

- `cargo` is at `C:\Users\julie\.cargo\bin` (not on default PATH);
  PowerShell needs `$env:PATH` prefixing, `2>$null` (not `/dev/null`).
- `cargo build --bin` (dev) cannot link here (gnu, no dlltool); gate ran
  the test-profile binary (`cargo build --profile test --bin`,
  `BIRCHTREE_BIN=target/debug/birchtree.exe`), smoke `cargo build --bin`
  lines no-op'd via a `cargo` shim in Temp (delegates everything else).
- git-bash `python3` shimmed at `~/bin/python3` → Python314 (smoke/lab
  heredocs). Machine-level, outside the repo; no repo scripts modified.
