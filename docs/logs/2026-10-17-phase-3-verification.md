# Verification evidence — 2026-10-17 Phase 3

Append-only. Command output captured from the release-relevant baseline (the
Phase 3 working tree, before commit). Machine: Linux.

## `bash scripts/smoke-all.sh`

The full gate runs `cargo check --all-targets`, `cargo test`,
`scripts/smoke-phase1.sh`, `scripts/smoke-phase2.sh`, and
`scripts/smoke-phase3.sh` (new in Phase 3). Exit status `0`; ends
`smoke-all: green`.

```
==> cargo check --all-targets
(no warnings; clean finish)

==> cargo test
   Compiling birchtree v0.1.0 (/root/birchtree)
    Finished `test` profile [optimized + debuginfo] target(s) in 8.45s
     Running unittests src/lib.rs (target/debug/deps/birchtree-792f57f948450e65)

running 51 tests
test api::forms::tests::collects_repeated_and_bracketed_keys ... ok
test api::forms::tests::decodes_percent_and_plus ... ok
test api::mastodon::errors::tests::domain_errors_map_to_statuses ... ok
test api::mastodon::pagination::tests::cursors_parse_numerically ... ok
test api::mastodon::errors::tests::error_body_is_error_key ... ok
test api::mastodon::pagination::tests::limit_is_clamped_to_max ... ok
test api::mastodon::pagination::tests::link_header_swaps_cursor_and_preserves_query ... ok
test api::mastodon::pagination::tests::negative_limit_is_rejected ... ok
test api::mastodon::rate_limits::tests::keys_are_isolated ... ok
test api::mastodon::request::tests::trusted_peer_without_header_falls_back ... ok
test api::mastodon::request::tests::trusted_peer_uses_leftmost_forwarded_address ... ok
test api::mastodon::rate_limits::tests::window_counts_and_resets ... ok
test api::oauth::pkce::tests::challenge_shape_is_bounded ... ok
test api::oauth::pkce::tests::only_s256_is_supported ... ok
test api::oauth::scopes::tests::known_scopes_include_broad_and_granular ... ok
test api::mastodon::request::tests::untrusted_peer_header_is_ignored ... ok
test api::oauth::scopes::tests::subset_detects_escalation ... ok
test api::oauth::scopes::tests::parse_dedupes_and_splits ... ok
test application::crypto::tests::pkce_s256_matches_rfc7636_vector ... ok
test application::accounts::tests::host_extraction_handles_common_urls ... ok
test application::policy::tests::blocked_viewer_cannot_fetch_public ... ok
test application::policy::tests::direct_is_author_or_mentioned_only ... ok
test application::policy::tests::public_is_fetchable_by_anyone ... ok
test application::policy::tests::quote_policy_gates_quote_permission ... ok
test application::crypto::tests::random_tokens_are_unique_and_hex ... ok
test config::tests::default_config_is_valid ... ok
test config::tests::rejects_zero_body_size ... ok
test config::tests::normalized_url_strips_trailing_slash ... ok
test domain::account::tests::acct_is_local_or_qualified ... ok
test domain::account::tests::normalize_is_ascii_lowercase ... ok
test domain::account::tests::username_validation_matches_mastodon_shape ... ok
test domain::id::tests::generator_advances_with_clock_and_never_goes_backwards ... ok
test application::policy::tests::private_requires_follow_or_mention ... ok
test domain::id::tests::id_deserializes_from_string_and_number ... ok
test application::crypto::tests::token_hash_is_deterministic_sha256 ... ok
test domain::id::tests::id_parse_rejects_non_digits ... ok
test config::tests::rejects_non_http_public_url ... ok
test domain::id::tests::id_serializes_as_decimal_string ... ok
test domain::id::tests::generator_is_strictly_increasing_within_millisecond ... ok
test domain::id::tests::numeric_order_matches_time_order_not_lexicographic ... ok
test domain::account::tests::handle_key_combines_parts ... ok
test domain::id::tests::round_trip_parts ... ok
test observability::tests::authorization_is_redacted ... ok
test persistence::redb::tests::rejects_world_writable_data_dir ... ok
test persistence::redb::migrations::tests::refuses_newer_schema ... ok
test persistence::redb::migrations::tests::fresh_database_migrates_to_target ... ok
test persistence::redb::tests::opens_and_migrates_fresh_database ... ok
test persistence::redb::migrations::tests::migration_is_idempotent_across_reopen ... ok
test persistence::redb::migrations::tests::upgrades_v2_database_to_v3 ... ok
test application::crypto::tests::password_hash_round_trips ... ok
test application::crypto::tests::hashes_are_salted ... ok

test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s

     Running unittests src/main.rs (target/debug/deps/birchtree-3e2b35525cffe151)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/crash_recovery.rs (target/debug/deps/crash_recovery-86cf0a56897627de)

running 3 tests
test aborted_open_leaves_no_partial_state ... ok
test cli_create_account_persists_and_is_readable ... ok
test committed_account_and_job_survive_sigkill ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

     Running tests/persistence_conformance.rs (target/debug/deps/persistence_conformance-8901197812db6280)

running 18 tests
test job_claim_complete_retry_and_quarantine ... ok
test create_and_read_back_account_and_credential ... ok
test account_creation_commits_one_follow_up_job_atomically ... ok
test authorization_code_redeem_is_single_use ... ok
test login_session_lifecycle_and_expiry ... ok
test oauth_app_and_token_lifecycle ... ok
test migrations_and_data_survive_restart ... ok
test authorization_code_redeem_is_atomic_under_contention ... ok
test crypto_helpers_reject_tampered_hash ... ok
test interaction_and_job_survive_store_reopen ... ok
test fan_out_notification_job_is_idempotent ... ok
test interaction_replay_does_not_duplicate_rows_or_jobs ... ok
test duplicate_handle_is_rejected_case_insensitively ... ok
test registration_commits_confirmation_mail_job_atomically ... ok
test unique_handle_race_has_exactly_one_winner ... ok
test status_idempotency_record_replays_within_hour ... ok
test status_create_commits_notification_and_poll_jobs_atomically ... ok
test password_authentication ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s

     Running tests/phase2_api.rs (target/debug/deps/phase2_api-81242cdbdda6e6cf)

running 13 tests
test instance_v1_shape ... ok
test registration_closed_returns_forbidden ... ok
test registration_needs_write_scope ... ok
test preferences_require_a_user ... ok
test instance_v2_shape_has_no_top_level_stats ... ok
test missing_token_is_unauthorized ... ok
test account_show_is_public_and_404s_unknown ... ok
test registration_needs_client_credentials ... ok
test registration_returns_user_token ... ok
test verify_credentials_returns_credential_shape ... ok
test update_credentials_persists_profile_and_settings ... ok
test duplicate_username_is_rejected ... ok
test relationships_returns_relationship_shape ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s

     Running tests/phase2_oauth.rs (target/debug/deps/phase2_oauth-98ecd878e58ac68f)

running 13 tests
test app_registration_returns_credential_shape ... ok
test app_registration_requires_name_and_redirect_uri ... ok
test invalid_client_secret_is_rejected ... ok
test revoke_empty_token_is_ok ... ok
test client_credentials_issues_app_only_token ... ok
test unsupported_grant_type_is_rejected ... ok
test revoke_invalidates_token ... ok
test client_credentials_rejects_scope_escalation ... ok
test revoke_with_wrong_client_is_forbidden ... ok
test authorization_code_is_single_use ... ok
test redirect_uri_mismatch_is_invalid_grant ... ok
test wrong_pkce_verifier_is_invalid_grant ... ok
test authorization_code_pkce_flow_issues_user_token ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

     Running tests/phase3_interactions.rs (target/debug/deps/phase3_interactions-56f4951e686abb80)

running 8 tests
test interaction_error_shapes ... ok
test bookmark_flags_are_per_viewer ... ok
test reblog_creates_wrapper_and_unreblog_restores ... ok
test interaction_visibility_gates ... ok
test favourite_lifecycle_and_counter_idempotency ... ok
test quote_lifecycle ... ok
test poll_vote_contract ... ok
test pin_rules_and_pinned_list ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s

     Running tests/phase3_relationships.rs (target/debug/deps/phase3_relationships-d80985505efe45d1)

running 13 tests
test self_follow_and_unknown_target_errors ... ok
test blocked_pair_cannot_follow ... ok
test follow_updates_relationship_and_stats ... ok
test block_removes_follows_and_endorsements_both_ways ... ok
test follow_unfollow_race_has_consistent_counters ... ok
test mute_duration_and_notifications ... ok
test relationship_dto_contract_shape ... ok
test endorsement_requires_follow_and_lists_work ... ok
test relationship_scopes_and_auth_errors ... ok
test locked_account_follow_request_lifecycle ... ok
test domain_blocks_crud_and_relationship_flag ... ok
test account_note_lifecycle ... ok
test unfollow_and_remove_from_followers_revert_counters ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s

     Running tests/phase3_status.rs (target/debug/deps/phase3_status-914ef4d5ac0d412f)

running 13 tests
test create_returns_contract_complete_status_dto ... ok
test create_validation_errors ... ok
test source_and_history_endpoints ... ok
test reply_and_quote_resolution_errors ... ok
test account_statuses_pinned_and_filters ... ok
test context_ancestors_descendants ... ok
test favourited_by_and_reblogged_by_lists ... ok
test viewer_fields_appear_only_with_token ... ok
test idempotency_key_replays_and_is_per_account ... ok
test multi_fetch ... ok
test edit_updates_and_bumps_edits ... ok
test delete_tombstones_and_returns_source ... ok
test visibility_policy_matrix ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s

     Running tests/runtime.rs (target/debug/deps/runtime-a7f85bff75005147)

running 3 tests
test ready_reports_storage_available ... ok
test health_is_ok ... ok
test unknown_route_is_404 ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/secret_handling.rs (target/debug/deps/secret_handling-adbdd1f5a6c6dca8)

running 3 tests
test authorization_headers_are_redacted ... ok
test serialized_token_record_omits_plaintext ... ok
test database_never_contains_plaintext_secrets ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

   Doc-tests birchtree

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```
==> scripts/smoke-phase1.sh
   Compiling birchtree v0.1.0 (/root/birchtree)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.82s
==> start server
[2m2026-10-17T06:24:28.702079Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker started
==> probes
==> graceful shutdown
[2m2026-10-17T06:24:28.734276Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m shutdown signal received
[2m2026-10-17T06:24:28.734622Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker stopped
[2m2026-10-17T06:24:28.734681Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m birchtree stopped
==> CLI create-account
created account id=117454816495992832 username=smoke
==> restart preserves identity
[2m2026-10-17T06:24:29.298200Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker started
[2m2026-10-17T06:24:29.300574Z[0m [32m INFO[0m [2mbirchtree::jobs::handlers[0m[2m:[0m user_created follow-up [3mjob[0m[2m=[0m117454816495992834 [3maccount_id[0m[2m=[0m117454816495992832
[2m2026-10-17T06:24:29.311919Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m shutdown signal received
[2m2026-10-17T06:24:29.312345Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker stopped
[2m2026-10-17T06:24:29.312427Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m birchtree stopped
SMOKE_OK
==> scripts/smoke-phase2.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
==> start server (registration open)
[2m2026-10-17T06:24:29.485366Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker started
==> instance endpoints
==> register OAuth application
==> client-credentials token
==> app-only token is rejected on a user endpoint
==> register account (needs the client-credentials token)
==> verify_credentials
[2m2026-10-17T06:24:30.242330Z[0m [32m INFO[0m [2mbirchtree::jobs::handlers[0m[2m:[0m user_created follow-up [3mjob[0m[2m=[0m117454816555892738 [3maccount_id[0m[2m=[0m117454816555892736
[2m2026-10-17T06:24:30.243463Z[0m [32m INFO[0m [2mbirchtree::mail[0m[2m:[0m confirmation email queued (logging adapter; no SMTP configured) [3maccount_id[0m[2m=[0m117454816555892736 [3mrecipient[0m[2m=[0msmokey@example.com [3mtoken_issued[0m[2m=[0mtrue
==> preferences
==> authorization-code + PKCE S256 flow
==> authorization code is single-use
==> revoke
==> restart with registration closed
[2m2026-10-17T06:24:30.991258Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m shutdown signal received
[2m2026-10-17T06:24:30.991535Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker stopped
[2m2026-10-17T06:24:30.991600Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m birchtree stopped
[2m2026-10-17T06:24:31.006757Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker started
==> graceful shutdown
[2m2026-10-17T06:24:31.043247Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m shutdown signal received
[2m2026-10-17T06:24:31.043569Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker stopped
[2m2026-10-17T06:24:31.043633Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m birchtree stopped
SMOKE_PHASE2_OK
==> scripts/smoke-phase3.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
==> start server
[2m2026-10-17T06:24:31.220109Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker started
==> setup: app + three accounts
[2m2026-10-17T06:24:31.976233Z[0m [32m INFO[0m [2mbirchtree::jobs::handlers[0m[2m:[0m user_created follow-up [3mjob[0m[2m=[0m117454816667435010 [3maccount_id[0m[2m=[0m117454816667435008
[2m2026-10-17T06:24:31.977387Z[0m [32m INFO[0m [2mbirchtree::mail[0m[2m:[0m confirmation email queued (logging adapter; no SMTP configured) [3maccount_id[0m[2m=[0m117454816667435008 [3mrecipient[0m[2m=[0malice@example.com [3mtoken_issued[0m[2m=[0mtrue
[2m2026-10-17T06:24:32.483606Z[0m [32m INFO[0m [2mbirchtree::jobs::handlers[0m[2m:[0m user_created follow-up [3mjob[0m[2m=[0m117454816704659458 [3maccount_id[0m[2m=[0m117454816704659456
[2m2026-10-17T06:24:32.484871Z[0m [32m INFO[0m [2mbirchtree::mail[0m[2m:[0m confirmation email queued (logging adapter; no SMTP configured) [3maccount_id[0m[2m=[0m117454816704659456 [3mrecipient[0m[2m=[0mbob@example.com [3mtoken_issued[0m[2m=[0mtrue
==> create a status (alice)
[2m2026-10-17T06:24:33.241848Z[0m [32m INFO[0m [2mbirchtree::jobs::handlers[0m[2m:[0m user_created follow-up [3mjob[0m[2m=[0m117454816741752834 [3maccount_id[0m[2m=[0m117454816741752832
[2m2026-10-17T06:24:33.242951Z[0m [32m INFO[0m [2mbirchtree::mail[0m[2m:[0m confirmation email queued (logging adapter; no SMTP configured) [3maccount_id[0m[2m=[0m117454816741752832 [3mrecipient[0m[2m=[0mcarol@example.com [3mtoken_issued[0m[2m=[0mtrue
==> anonymous show + 404 shape
==> favourite is idempotent
==> bookmark + reblog
==> reply + context
==> Idempotency-Key replays the created status
==> multi-fetch + over-limit
==> edit + history + source
==> follow loop (bob -> alice) with real relationship flags
==> locked account follow request
==> note + endorsement + block/mute
==> visibility boundaries
==> pin + pinned list
==> poll create + vote
==> quote create + revoke + interaction policy
==> domain blocks
==> delete returns source and 404s afterwards
==> graceful shutdown
[2m2026-10-17T06:24:35.349677Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m shutdown signal received
[2m2026-10-17T06:24:35.349975Z[0m [32m INFO[0m [2mbirchtree::jobs::worker[0m[2m:[0m job worker stopped
[2m2026-10-17T06:24:35.350038Z[0m [32m INFO[0m [2mbirchtree[0m[2m:[0m birchtree stopped
SMOKE_PHASE3_OK
smoke-all: green
```

## Phase 3 suite breakdown

- `tests/phase3_status.rs` — 13 tests: exact Status DTO key set/nullability,
  viewer-only fields, validation strings (blank text, 500/URL=23, poll
  `Choices`/`Deadline` rules), reply/quote 404 messages, visibility matrix
  (public/unlisted/private/direct x anonymous/author/follower/mentioned/
  blocked), context ordering, source/history exact serializers, multi-fetch
  (dedup, <=20, silent omission, id-desc), `Idempotency-Key` replay +
  per-account namespacing, delete tombstone + source-mode body + cascade
  counters, edit/no-op-edit semantics, account statuses + `pinned=true` +
  `Link` headers, favourited_by/reblogged_by lists.
- `tests/phase3_relationships.rs` — 13 tests: exact RelationshipSerializer
  key set + neutral defaults, follow/unfollow/remove-follower counters,
  self-follow 403, locked-account request lifecycle (list/authorize/reject),
  block severing mutual follows + endorsements, blocked-pair 403, mute
  duration/notifications, notes (2000 boundary, blank deletes, privacy),
  endorsement-follow requirement + lists, domain-block CRUD + relationship
  flag, scope/auth error shapes, 8-way concurrent follow/unfollow race with
  exact counters.
- `tests/phase3_interactions.rs` — 8 tests: favourite lifecycle +
  counter idempotency + per-viewer independence, bookmarks, reblog wrapper +
  unreblog + idempotent re-reblog (proper-resolved `reblogged`/`favourited`),
  interaction visibility gates (404 vs 403), pin rules (four exact 422
  messages + limit 5 + pinned list/flag), poll vote contract (400 missing
  `choices`, four 422s, multiple-poll add-a-choice, hide_totals masking),
  quote lifecycle (auto-accept, shallow nesting, counts, revoke, policy
  round-trip), 401/403/422 auth error shapes.
- `tests/persistence_conformance.rs` — +5 tests: status create commits
  record + indexes + counters + jobs atomically (aborted create leaves
  nothing), fan-out job dedup (retry -> one notification, fresh interaction ->
  new), interaction replay -> one row/one job, `Idempotency-Key` replay within
  the hour (same id, one job set, stats bumped once), interaction + job
  survive store reopen and post-restart completion yields exactly one
  notification.

## Product bugs found by the Phase 3 suites and fixed

1. Bodyless POSTs (`reblog`, `follow`, ...) answered `415` (`RawForm`) — added
   `api::forms::LenientForm`.
2. `can_fetch` returned true for public/unlisted even when the author blocked
   the viewer.
3. Neutral relationship `showing_reblogs` defaulted to `true`; the pinned
   serializer defaults to `false` without a row.
4. Single-choice polls accepted a second (different) vote; pinned
   `VoteValidator` rejects any second vote on a non-multiple poll.
5. `GET /api/v1/follow_requests` accepted `read` without `follow`; pinned
   controller lists `:follow, :read, :'read:follows'`.
6. Multi-fetch did not deduplicate repeated `id[]` (Mastodon `.uniq`).
7. `status_source_json` emitted `"spoiler_text": null` (column is NOT NULL
   DEFAULT '' in the pinned schema).
8. Poll expiry/option errors used fabricated strings; aligned to the pinned
   full messages: `Validation failed: Deadline is too soon` / `Deadline can't
   be blank` / `Deadline is too far into the future` / `Choices must have
   more than one item` / `Choices can't contain more than 4 items` / `Choices
   cannot be longer than 50 characters each` / `Choices contain duplicate
   items`.
9. Pinning someone else's post answered `403` without the prefix; pinned
   source answers 422 with `Validation failed: Someone else's post cannot be
   pinned` (StatusPinValidator -> RecordInvalid).
10. Viewer flags were dead on reblog wrappers: `favourited`/`bookmarked`/
    `reblogged` must resolve through `status.proper`
    (`Account#interactions`), `pinned` row-exact; `status_view_flags` now
    proper-resolves.
11. `unreblog` left the `REBLOGS_BY_ACCOUNT` mapping behind, so `reblogged`
    stayed `true`; the mapping is removed with the tombstone.

## Not covered / deferred

See `docs/phase-3-exit-criteria.md` "Open / deferred": media bytes and
serving, timelines, notification read surface (Phase 4), streaming (Phase 5),
federation incl. remote quote requests (Phase 6), JSON request bodies and
collection reads `blocks/mutes/favourites/bookmarks` (Phase 7).
