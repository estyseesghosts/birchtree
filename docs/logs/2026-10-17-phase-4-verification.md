# Phase 4 verification evidence — 2026-10-17

Append-only evidence for the Phase 4 exit (`docs/phase-4-exit-criteria.md`).
Baseline: `master` after commits `e7beac3` (hashtag index decode fix) and
`f5dd20a` (smoke-phase4 assertion fixes) on top of `53eb3dd`; the timelines
slice follows in the Phase 4 completion commit. Output below is pasted from
the runs, ANSI colour codes and tracing timestamps removed where noted.

## 1. `cargo check --all-targets` — zero warnings

```
    Checking birchtree v0.1.0 (/root/birchtree)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.24s
```

## 2. `cargo test` — 219 passed, 0 failed

Per-suite counts (`Running` line, then its result):

```
     Running unittests src/lib.rs
test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/crash_recovery.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
     Running tests/persistence_conformance.rs
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s
     Running tests/phase2_api.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
     Running tests/phase2_oauth.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
     Running tests/phase3_interactions.rs
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
     Running tests/phase3_relationships.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
     Running tests/phase3_status.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
     Running tests/phase4_filters.rs
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
     Running tests/phase4_lists.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
     Running tests/phase4_media.rs
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
     Running tests/phase4_notifications.rs
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
     Running tests/phase4_scheduled.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
     Running tests/phase4_search_emoji.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running tests/phase4_timelines.rs
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
     Running tests/runtime.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/secret_handling.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
   Doc-tests birchtree
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Full `cargo test` output:

```
   Compiling birchtree v0.1.0 (/root/birchtree)
    Finished `test` profile [optimized + debuginfo] target(s) in 25.52s
     Running unittests src/lib.rs (target/debug/deps/birchtree-84c2a5783b3de532)

running 71 tests
test api::forms::tests::decodes_percent_and_plus ... ok
test api::forms::tests::collects_repeated_and_bracketed_keys ... ok
test api::mastodon::errors::tests::domain_errors_map_to_statuses ... ok
test api::mastodon::pagination::tests::cursors_parse_numerically ... ok
test api::mastodon::pagination::tests::link_header_swaps_cursor_and_preserves_query ... ok
test api::mastodon::errors::tests::error_body_is_error_key ... ok
test api::mastodon::pagination::tests::negative_limit_is_rejected ... ok
test api::mastodon::pagination::tests::limit_is_clamped_to_max ... ok
test api::mastodon::rate_limits::tests::keys_are_isolated ... ok
test api::mastodon::rate_limits::tests::window_counts_and_resets ... ok
test api::mastodon::request::tests::trusted_peer_uses_leftmost_forwarded_address ... ok
test api::mastodon::request::tests::trusted_peer_without_header_falls_back ... ok
test api::mastodon::request::tests::untrusted_peer_header_is_ignored ... ok
test api::oauth::pkce::tests::challenge_shape_is_bounded ... ok
test api::oauth::pkce::tests::only_s256_is_supported ... ok
test api::oauth::scopes::tests::known_scopes_include_broad_and_granular ... ok
test api::oauth::scopes::tests::parse_dedupes_and_splits ... ok
test api::oauth::scopes::tests::subset_detects_escalation ... ok
test application::accounts::tests::host_extraction_handles_common_urls ... ok
test application::crypto::tests::pkce_s256_matches_rfc7636_vector ... ok
test application::crypto::tests::random_tokens_are_unique_and_hex ... ok
test application::crypto::tests::token_hash_is_deterministic_sha256 ... ok
test application::media::tests::focus_parses_floats_and_rejects_garbage ... ok
test application::media::tests::human_size_formats_like_rails_number_to_human_size ... ok
test application::policy::tests::blocked_viewer_cannot_fetch_public ... ok
test application::policy::tests::direct_is_author_or_mentioned_only ... ok
test application::policy::tests::private_requires_follow_or_mention ... ok
test application::policy::tests::public_is_fetchable_by_anyone ... ok
test application::policy::tests::quote_policy_gates_quote_permission ... ok
test config::tests::default_config_is_valid ... ok
test config::tests::normalized_url_strips_trailing_slash ... ok
test config::tests::rejects_zero_body_size ... ok
test config::tests::rejects_non_http_public_url ... ok
test application::media::tests::blank_and_unaccepted_uploads_are_rejected_before_staging ... ok
test domain::account::tests::handle_key_combines_parts ... ok
test application::media::tests::size_limit_message_reports_the_configured_cap ... ok
test domain::account::tests::normalize_is_ascii_lowercase ... ok
test domain::account::tests::acct_is_local_or_qualified ... ok
test application::media::tests::thumbnails_are_video_audio_only ... ok
test domain::id::tests::id_parse_rejects_non_digits ... ok
test domain::id::tests::id_serializes_as_decimal_string ... ok
test application::media::tests::image_upload_processes_inline_and_is_idempotent ... ok
test domain::account::tests::username_validation_matches_mastodon_shape ... ok
test domain::id::tests::generator_advances_with_clock_and_never_goes_backwards ... ok
test domain::id::tests::id_deserializes_from_string_and_number ... ok
test domain::id::tests::numeric_order_matches_time_order_not_lexicographic ... ok
test domain::id::tests::generator_is_strictly_increasing_within_millisecond ... ok
test domain::id::tests::round_trip_parts ... ok
test media::ingest::tests::accepted_list_matches_the_pinned_upload_mime_types ... ok
test application::media::tests::delayed_mp4_stays_pending_until_the_job_runs ... ok
test media::ingest::tests::classifies_common_formats ... ok
test media::blob_store::tests::round_trips_and_rejects_traversal ... ok
test media::processing::tests::audio_and_webm_are_accepted_without_dimensions ... ok
test media::processing::tests::fit_within_never_upscales_or_produces_zero ... ok
test media::processing::tests::corrupt_or_unsupported_bytes_are_unidentifiable ... ok
test media::ingest::tests::rejects_unknown_and_short_input ... ok
test media::processing::tests::mp4_dimensions_from_tkhd_box ... ok
test observability::tests::authorization_is_redacted ... ok
test application::media::tests::update_and_delete_round_trip ... ok
test media::processing::tests::png_round_trip_reports_dims_blurhash_and_preview ... ok
test media::processing::tests::jpeg_and_webp_keep_their_format_in_the_preview ... ok
test persistence::redb::tests::rejects_world_writable_data_dir ... ok
test persistence::redb::migrations::tests::fresh_database_migrates_to_target ... ok
test media::processing::tests::gif_and_bmp_previews_are_png ... ok
test persistence::redb::migrations::tests::migration_is_idempotent_across_reopen ... ok
test persistence::redb::tests::opens_and_migrates_fresh_database ... ok
test persistence::redb::migrations::tests::refuses_newer_schema ... ok
test persistence::redb::migrations::tests::upgrades_v2_database_to_v3 ... ok
test media::processing::tests::large_images_scale_down_to_the_preview_bound ... ok
test application::crypto::tests::password_hash_round_trips ... ok
test application::crypto::tests::hashes_are_salted ... ok

test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running unittests src/main.rs (target/debug/deps/birchtree-5128dabda0e4ac1e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/crash_recovery.rs (target/debug/deps/crash_recovery-ec71ce48cb7cc63e)

running 3 tests
test aborted_open_leaves_no_partial_state ... ok
test cli_create_account_persists_and_is_readable ... ok
test committed_account_and_job_survive_sigkill ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

     Running tests/persistence_conformance.rs (target/debug/deps/persistence_conformance-5d6cae020b62d0c9)

running 18 tests
test job_claim_complete_retry_and_quarantine ... ok
test account_creation_commits_one_follow_up_job_atomically ... ok
test login_session_lifecycle_and_expiry ... ok
test authorization_code_redeem_is_single_use ... ok
test oauth_app_and_token_lifecycle ... ok
test migrations_and_data_survive_restart ... ok
test create_and_read_back_account_and_credential ... ok
test authorization_code_redeem_is_atomic_under_contention ... ok
test crypto_helpers_reject_tampered_hash ... ok
test registration_commits_confirmation_mail_job_atomically ... ok
test interaction_and_job_survive_store_reopen ... ok
test fan_out_notification_job_is_idempotent ... ok
test duplicate_handle_is_rejected_case_insensitively ... ok
test interaction_replay_does_not_duplicate_rows_or_jobs ... ok
test unique_handle_race_has_exactly_one_winner ... ok
test status_idempotency_record_replays_within_hour ... ok
test status_create_commits_notification_and_poll_jobs_atomically ... ok
test password_authentication ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s

     Running tests/phase2_api.rs (target/debug/deps/phase2_api-4352709d9d418194)

running 13 tests
test instance_v2_shape_has_no_top_level_stats ... ok
test missing_token_is_unauthorized ... ok
test preferences_require_a_user ... ok
test registration_closed_returns_forbidden ... ok
test registration_needs_write_scope ... ok
test instance_v1_shape ... ok
test verify_credentials_returns_credential_shape ... ok
test registration_needs_client_credentials ... ok
test account_show_is_public_and_404s_unknown ... ok
test registration_returns_user_token ... ok
test update_credentials_persists_profile_and_settings ... ok
test relationships_returns_relationship_shape ... ok
test duplicate_username_is_rejected ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/phase2_oauth.rs (target/debug/deps/phase2_oauth-9e9ad6181dde8a27)

running 13 tests
test revoke_with_wrong_client_is_forbidden ... ok
test revoke_invalidates_token ... ok
test revoke_empty_token_is_ok ... ok
test app_registration_requires_name_and_redirect_uri ... ok
test invalid_client_secret_is_rejected ... ok
test app_registration_returns_credential_shape ... ok
test client_credentials_issues_app_only_token ... ok
test client_credentials_rejects_scope_escalation ... ok
test unsupported_grant_type_is_rejected ... ok
test authorization_code_is_single_use ... ok
test redirect_uri_mismatch_is_invalid_grant ... ok
test authorization_code_pkce_flow_issues_user_token ... ok
test wrong_pkce_verifier_is_invalid_grant ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

     Running tests/phase3_interactions.rs (target/debug/deps/phase3_interactions-368fea942d43fe09)

running 8 tests
test interaction_error_shapes ... ok
test interaction_visibility_gates ... ok
test reblog_creates_wrapper_and_unreblog_restores ... ok
test bookmark_flags_are_per_viewer ... ok
test favourite_lifecycle_and_counter_idempotency ... ok
test poll_vote_contract ... ok
test quote_lifecycle ... ok
test pin_rules_and_pinned_list ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s

     Running tests/phase3_relationships.rs (target/debug/deps/phase3_relationships-df28bd7ec3401a2b)

running 13 tests
test self_follow_and_unknown_target_errors ... ok
test follow_updates_relationship_and_stats ... ok
test block_removes_follows_and_endorsements_both_ways ... ok
test relationship_dto_contract_shape ... ok
test relationship_scopes_and_auth_errors ... ok
test blocked_pair_cannot_follow ... ok
test endorsement_requires_follow_and_lists_work ... ok
test mute_duration_and_notifications ... ok
test follow_unfollow_race_has_consistent_counters ... ok
test locked_account_follow_request_lifecycle ... ok
test domain_blocks_crud_and_relationship_flag ... ok
test account_note_lifecycle ... ok
test unfollow_and_remove_from_followers_revert_counters ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s

     Running tests/phase3_status.rs (target/debug/deps/phase3_status-fa06b3834efc01a9)

running 13 tests
test create_validation_errors ... ok
test source_and_history_endpoints ... ok
test create_returns_contract_complete_status_dto ... ok
test reply_and_quote_resolution_errors ... ok
test edit_updates_and_bumps_edits ... ok
test favourited_by_and_reblogged_by_lists ... ok
test idempotency_key_replays_and_is_per_account ... ok
test multi_fetch ... ok
test delete_tombstones_and_returns_source ... ok
test viewer_fields_appear_only_with_token ... ok
test context_ancestors_descendants ... ok
test account_statuses_pinned_and_filters ... ok
test visibility_policy_matrix ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s

     Running tests/phase4_filters.rs (target/debug/deps/phase4_filters-fec086c412e1ff84)

running 15 tests
test v1_create_makes_filter_and_keyword_atomically ... ok
test v2_create_returns_exact_key_set_and_round_trips ... ok
test v1_update_multi_keyword_guard ... ok
test v1_destroy_removes_only_the_keyword ... ok
test v1_update_patches_single_keyword_filters ... ok
test v1_index_projects_keyword_rows ... ok
test status_subresource_adds_and_removes_rules ... ok
test filter_results_matches_keywords_with_exact_payload ... ok
test filter_results_status_rules_polls_and_reblogs ... ok
test v2_create_validation_messages_are_exact ... ok
test v2_update_patches_fields_clears_expiry_and_replaces_keywords ... ok
test keyword_subresource_crud_with_exact_key_sets ... ok
test wrong_scope_is_forbidden ... ok
test foreign_filters_answer_record_not_found ... ok
test filter_results_whole_word_expiry_and_context ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s

     Running tests/phase4_lists.rs (target/debug/deps/phase4_lists-3efc17f0d8b9a1cb)

running 5 tests
test list_scopes_and_auth_are_enforced ... ok
test list_crud_round_trip_with_exact_keys ... ok
test list_validation_bodies_match_mastodon ... ok
test other_users_lists_are_404 ... ok
test membership_requires_follow_and_cascades_on_unfollow ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

     Running tests/phase4_media.rs (target/debug/deps/phase4_media-f44a1b41007b6d68)

running 8 tests
test granular_write_media_scope_is_accepted ... ok
test upload_requires_write_scope_and_a_user_token ... ok
test v1_png_upload_is_processed_and_served ... ok
test blank_and_unaccepted_files_are_rejected ... ok
test v2_mp4_upload_is_delayed_then_processed_by_the_job ... ok
test description_and_focus_round_trip_through_update ... ok
test destroy_removes_unattached_media_but_rejects_attached ... ok
test foreign_media_is_not_found ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/phase4_notifications.rs (target/debug/deps/phase4_notifications-5f6242da655332ad)

running 4 tests
test requests_surface_is_honest_and_scopes_are_enforced ... ok
test markers_track_last_read_and_bump_versions ... ok
test show_dismiss_clear_and_unread_count ... ok
test index_shows_interactions_with_exact_shape_and_filters ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s

     Running tests/phase4_scheduled.rs (target/debug/deps/phase4_scheduled-46724f4820d4984b)

running 3 tests
test delete_scheduled_status_returns_empty_object ... ok
test scheduling_returns_the_scheduled_shape_and_publishes_later ... ok
test scheduling_validation_matches_mastodon ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

     Running tests/phase4_search_emoji.rs (target/debug/deps/phase4_search_emoji-9a58119bce0dc856)

running 3 tests
test search_requires_q_and_pages_with_mastodon_messages ... ok
test emoji_index_is_public_and_serves_images ... ok
test search_finds_accounts_hashtags_and_statuses ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

     Running tests/phase4_timelines.rs (target/debug/deps/phase4_timelines-b13576db7787f80e)

running 23 tests
test home_requires_a_user_token ... ok
test cache_control_follows_the_feed_and_the_viewer ... ok
test home_show_reblogs_false_hides_that_follows_reblogs ... ok
test home_direct_statuses_reach_only_the_mentioned_viewer ... ok
test home_orders_own_and_followed_statuses_newest_first ... ok
test home_private_statuses_need_a_follow ... ok
test home_own_statuses_always_appear_even_when_direct ... ok
test home_excludes_blocked_authors_in_both_directions ... ok
test home_timeline_carries_the_filtered_annotation ... ok
test home_hides_muted_authors_and_reblogs_of_muted_targets ... ok
test domain_blocked_authors_are_excluded_for_the_viewer_only ... ok
test tag_timeline_unknown_tag_is_an_empty_array_without_link ... ok
test cursors_limit_and_link_headers_match_the_contract ... ok
test home_reply_rules_follow_the_parent ... ok
test list_replies_policy_list_keeps_replies_to_members ... ok
test list_errors_are_exact ... ok
test list_replies_policy_followed_and_none ... ok
test public_local_flag_and_only_media_scope_the_feed ... ok
test public_excludes_blocked_blocked_by_and_muted_authors_for_the_viewer ... ok
test public_is_public_only_with_self_replies_and_no_reblogs ... ok
test list_timeline_shows_member_statuses_only ... ok
test tag_timeline_normalizes_case_and_keeps_replies ... ok
test sparse_filter_scan_stays_bounded ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s

     Running tests/runtime.rs (target/debug/deps/runtime-4f8d478edf59824c)

running 3 tests
test unknown_route_is_404 ... ok
test health_is_ok ... ok
test ready_reports_storage_available ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/secret_handling.rs (target/debug/deps/secret_handling-74f0497b97e57f79)

running 3 tests
test authorization_headers_are_redacted ... ok
test serialized_token_record_omits_plaintext ... ok
test database_never_contains_plaintext_secrets ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

   Doc-tests birchtree

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## 3. `tests/phase4_timelines.rs` — 23 tests (new suite)

Targeted measurement, run with `--nocapture` (the bounded-scan numbers):

```
sparse_filter_scan_stays_bounded: items_scanned=286 read_txns=13
```

Assertions: `items_scanned <= 5_000` and `read_txns <= 600`. The 20th
matching status is the 286th index entry from the newest (300 candidates,
bob muted, carol supplies the page); 13 read transactions = one scan, two
lookups per distinct author seen (bob, carol), and the constant render loads.

## 4. `scripts/smoke-phase4.sh` (standalone run)

```
==> start server
==> setup: app + two accounts
==> media: v1 upload (multipart) -> 200 with processed urls
==> media: serve the stored bytes
==> media: update description/focus
==> media: v2 upload of a video-ish file -> 202 while processing
==> media: rejected uploads
==> post a status with the attachment
==> follow + timelines
==> notifications + markers
==> scheduled status
==> search
==> custom emoji index (public, empty)
==> Vary header on API responses
SMOKE_PHASE4_OK
```

## 5. `bash scripts/smoke-all.sh` — `smoke-all: green`

Full output (check + test + phase1..phase4 smokes):

```
==> cargo check --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.25s
==> cargo test
    Finished `test` profile [optimized + debuginfo] target(s) in 0.20s
     Running unittests src/lib.rs (target/debug/deps/birchtree-84c2a5783b3de532)

running 71 tests
test api::forms::tests::decodes_percent_and_plus ... ok
test api::forms::tests::collects_repeated_and_bracketed_keys ... ok
test api::mastodon::errors::tests::domain_errors_map_to_statuses ... ok
test api::mastodon::pagination::tests::cursors_parse_numerically ... ok
test api::mastodon::pagination::tests::limit_is_clamped_to_max ... ok
test api::mastodon::errors::tests::error_body_is_error_key ... ok
test api::mastodon::pagination::tests::link_header_swaps_cursor_and_preserves_query ... ok
test api::mastodon::pagination::tests::negative_limit_is_rejected ... ok
test api::mastodon::rate_limits::tests::keys_are_isolated ... ok
test api::mastodon::rate_limits::tests::window_counts_and_resets ... ok
test api::mastodon::request::tests::trusted_peer_uses_leftmost_forwarded_address ... ok
test api::mastodon::request::tests::trusted_peer_without_header_falls_back ... ok
test api::mastodon::request::tests::untrusted_peer_header_is_ignored ... ok
test api::oauth::pkce::tests::challenge_shape_is_bounded ... ok
test api::oauth::pkce::tests::only_s256_is_supported ... ok
test api::oauth::scopes::tests::known_scopes_include_broad_and_granular ... ok
test api::oauth::scopes::tests::parse_dedupes_and_splits ... ok
test api::oauth::scopes::tests::subset_detects_escalation ... ok
test application::accounts::tests::host_extraction_handles_common_urls ... ok
test application::crypto::tests::random_tokens_are_unique_and_hex ... ok
test application::crypto::tests::pkce_s256_matches_rfc7636_vector ... ok
test application::crypto::tests::token_hash_is_deterministic_sha256 ... ok
test application::media::tests::focus_parses_floats_and_rejects_garbage ... ok
test application::media::tests::human_size_formats_like_rails_number_to_human_size ... ok
test application::policy::tests::blocked_viewer_cannot_fetch_public ... ok
test application::policy::tests::direct_is_author_or_mentioned_only ... ok
test application::policy::tests::public_is_fetchable_by_anyone ... ok
test application::policy::tests::quote_policy_gates_quote_permission ... ok
test config::tests::default_config_is_valid ... ok
test config::tests::rejects_non_http_public_url ... ok
test application::media::tests::blank_and_unaccepted_uploads_are_rejected_before_staging ... ok
test config::tests::rejects_zero_body_size ... ok
test application::policy::tests::private_requires_follow_or_mention ... ok
test domain::account::tests::acct_is_local_or_qualified ... ok
test config::tests::normalized_url_strips_trailing_slash ... ok
test application::media::tests::thumbnails_are_video_audio_only ... ok
test domain::account::tests::handle_key_combines_parts ... ok
test application::media::tests::size_limit_message_reports_the_configured_cap ... ok
test domain::account::tests::normalize_is_ascii_lowercase ... ok
test domain::account::tests::username_validation_matches_mastodon_shape ... ok
test domain::id::tests::generator_advances_with_clock_and_never_goes_backwards ... ok
test application::media::tests::delayed_mp4_stays_pending_until_the_job_runs ... ok
test domain::id::tests::id_deserializes_from_string_and_number ... ok
test domain::id::tests::id_serializes_as_decimal_string ... ok
test domain::id::tests::generator_is_strictly_increasing_within_millisecond ... ok
test domain::id::tests::round_trip_parts ... ok
test domain::id::tests::numeric_order_matches_time_order_not_lexicographic ... ok
test domain::id::tests::id_parse_rejects_non_digits ... ok
test media::ingest::tests::accepted_list_matches_the_pinned_upload_mime_types ... ok
test media::ingest::tests::classifies_common_formats ... ok
test media::ingest::tests::rejects_unknown_and_short_input ... ok
test media::blob_store::tests::round_trips_and_rejects_traversal ... ok
test application::media::tests::image_upload_processes_inline_and_is_idempotent ... ok
test media::processing::tests::audio_and_webm_are_accepted_without_dimensions ... ok
test application::media::tests::update_and_delete_round_trip ... ok
test media::processing::tests::corrupt_or_unsupported_bytes_are_unidentifiable ... ok
test media::processing::tests::fit_within_never_upscales_or_produces_zero ... ok
test observability::tests::authorization_is_redacted ... ok
test media::processing::tests::mp4_dimensions_from_tkhd_box ... ok
test media::processing::tests::jpeg_and_webp_keep_their_format_in_the_preview ... ok
test media::processing::tests::png_round_trip_reports_dims_blurhash_and_preview ... ok
test persistence::redb::tests::rejects_world_writable_data_dir ... ok
test media::processing::tests::gif_and_bmp_previews_are_png ... ok
test persistence::redb::migrations::tests::fresh_database_migrates_to_target ... ok
test persistence::redb::migrations::tests::refuses_newer_schema ... ok
test persistence::redb::tests::opens_and_migrates_fresh_database ... ok
test persistence::redb::migrations::tests::migration_is_idempotent_across_reopen ... ok
test persistence::redb::migrations::tests::upgrades_v2_database_to_v3 ... ok
test media::processing::tests::large_images_scale_down_to_the_preview_bound ... ok
test application::crypto::tests::password_hash_round_trips ... ok
test application::crypto::tests::hashes_are_salted ... ok

test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running unittests src/main.rs (target/debug/deps/birchtree-5128dabda0e4ac1e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/crash_recovery.rs (target/debug/deps/crash_recovery-ec71ce48cb7cc63e)

running 3 tests
test aborted_open_leaves_no_partial_state ... ok
test committed_account_and_job_survive_sigkill ... ok
test cli_create_account_persists_and_is_readable ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

     Running tests/persistence_conformance.rs (target/debug/deps/persistence_conformance-5d6cae020b62d0c9)

running 18 tests
test job_claim_complete_retry_and_quarantine ... ok
test authorization_code_redeem_is_single_use ... ok
test create_and_read_back_account_and_credential ... ok
test migrations_and_data_survive_restart ... ok
test login_session_lifecycle_and_expiry ... ok
test account_creation_commits_one_follow_up_job_atomically ... ok
test authorization_code_redeem_is_atomic_under_contention ... ok
test oauth_app_and_token_lifecycle ... ok
test interaction_replay_does_not_duplicate_rows_or_jobs ... ok
test interaction_and_job_survive_store_reopen ... ok
test crypto_helpers_reject_tampered_hash ... ok
test fan_out_notification_job_is_idempotent ... ok
test duplicate_handle_is_rejected_case_insensitively ... ok
test registration_commits_confirmation_mail_job_atomically ... ok
test status_idempotency_record_replays_within_hour ... ok
test unique_handle_race_has_exactly_one_winner ... ok
test status_create_commits_notification_and_poll_jobs_atomically ... ok
test password_authentication ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.36s

     Running tests/phase2_api.rs (target/debug/deps/phase2_api-4352709d9d418194)

running 13 tests
test preferences_require_a_user ... ok
test registration_needs_write_scope ... ok
test missing_token_is_unauthorized ... ok
test registration_closed_returns_forbidden ... ok
test instance_v2_shape_has_no_top_level_stats ... ok
test instance_v1_shape ... ok
test account_show_is_public_and_404s_unknown ... ok
test registration_needs_client_credentials ... ok
test update_credentials_persists_profile_and_settings ... ok
test verify_credentials_returns_credential_shape ... ok
test registration_returns_user_token ... ok
test duplicate_username_is_rejected ... ok
test relationships_returns_relationship_shape ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/phase2_oauth.rs (target/debug/deps/phase2_oauth-9e9ad6181dde8a27)

running 13 tests
test revoke_invalidates_token ... ok
test app_registration_returns_credential_shape ... ok
test invalid_client_secret_is_rejected ... ok
test client_credentials_issues_app_only_token ... ok
test revoke_with_wrong_client_is_forbidden ... ok
test revoke_empty_token_is_ok ... ok
test app_registration_requires_name_and_redirect_uri ... ok
test client_credentials_rejects_scope_escalation ... ok
test unsupported_grant_type_is_rejected ... ok
test authorization_code_pkce_flow_issues_user_token ... ok
test authorization_code_is_single_use ... ok
test redirect_uri_mismatch_is_invalid_grant ... ok
test wrong_pkce_verifier_is_invalid_grant ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/phase3_interactions.rs (target/debug/deps/phase3_interactions-368fea942d43fe09)

running 8 tests
test bookmark_flags_are_per_viewer ... ok
test reblog_creates_wrapper_and_unreblog_restores ... ok
test interaction_error_shapes ... ok
test interaction_visibility_gates ... ok
test quote_lifecycle ... ok
test poll_vote_contract ... ok
test favourite_lifecycle_and_counter_idempotency ... ok
test pin_rules_and_pinned_list ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s

     Running tests/phase3_relationships.rs (target/debug/deps/phase3_relationships-df28bd7ec3401a2b)

running 13 tests
test self_follow_and_unknown_target_errors ... ok
test relationship_dto_contract_shape ... ok
test follow_updates_relationship_and_stats ... ok
test mute_duration_and_notifications ... ok
test relationship_scopes_and_auth_errors ... ok
test block_removes_follows_and_endorsements_both_ways ... ok
test follow_unfollow_race_has_consistent_counters ... ok
test blocked_pair_cannot_follow ... ok
test endorsement_requires_follow_and_lists_work ... ok
test account_note_lifecycle ... ok
test domain_blocks_crud_and_relationship_flag ... ok
test locked_account_follow_request_lifecycle ... ok
test unfollow_and_remove_from_followers_revert_counters ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.29s

     Running tests/phase3_status.rs (target/debug/deps/phase3_status-fa06b3834efc01a9)

running 13 tests
test create_validation_errors ... ok
test reply_and_quote_resolution_errors ... ok
test create_returns_contract_complete_status_dto ... ok
test source_and_history_endpoints ... ok
test edit_updates_and_bumps_edits ... ok
test delete_tombstones_and_returns_source ... ok
test context_ancestors_descendants ... ok
test favourited_by_and_reblogged_by_lists ... ok
test multi_fetch ... ok
test idempotency_key_replays_and_is_per_account ... ok
test viewer_fields_appear_only_with_token ... ok
test account_statuses_pinned_and_filters ... ok
test visibility_policy_matrix ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.27s

     Running tests/phase4_filters.rs (target/debug/deps/phase4_filters-fec086c412e1ff84)

running 15 tests
test filter_results_matches_keywords_with_exact_payload ... ok
test status_subresource_adds_and_removes_rules ... ok
test v2_create_returns_exact_key_set_and_round_trips ... ok
test v1_create_makes_filter_and_keyword_atomically ... ok
test v1_destroy_removes_only_the_keyword ... ok
test v1_update_patches_single_keyword_filters ... ok
test v1_index_projects_keyword_rows ... ok
test v1_update_multi_keyword_guard ... ok
test filter_results_status_rules_polls_and_reblogs ... ok
test v2_create_validation_messages_are_exact ... ok
test foreign_filters_answer_record_not_found ... ok
test keyword_subresource_crud_with_exact_key_sets ... ok
test v2_update_patches_fields_clears_expiry_and_replaces_keywords ... ok
test wrong_scope_is_forbidden ... ok
test filter_results_whole_word_expiry_and_context ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s

     Running tests/phase4_lists.rs (target/debug/deps/phase4_lists-3efc17f0d8b9a1cb)

running 5 tests
test list_scopes_and_auth_are_enforced ... ok
test list_crud_round_trip_with_exact_keys ... ok
test list_validation_bodies_match_mastodon ... ok
test other_users_lists_are_404 ... ok
test membership_requires_follow_and_cascades_on_unfollow ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s

     Running tests/phase4_media.rs (target/debug/deps/phase4_media-f44a1b41007b6d68)

running 8 tests
test granular_write_media_scope_is_accepted ... ok
test upload_requires_write_scope_and_a_user_token ... ok
test description_and_focus_round_trip_through_update ... ok
test blank_and_unaccepted_files_are_rejected ... ok
test v1_png_upload_is_processed_and_served ... ok
test v2_mp4_upload_is_delayed_then_processed_by_the_job ... ok
test destroy_removes_unattached_media_but_rejects_attached ... ok
test foreign_media_is_not_found ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s

     Running tests/phase4_notifications.rs (target/debug/deps/phase4_notifications-5f6242da655332ad)

running 4 tests
test requests_surface_is_honest_and_scopes_are_enforced ... ok
test index_shows_interactions_with_exact_shape_and_filters ... ok
test markers_track_last_read_and_bump_versions ... ok
test show_dismiss_clear_and_unread_count ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

     Running tests/phase4_scheduled.rs (target/debug/deps/phase4_scheduled-46724f4820d4984b)

running 3 tests
test delete_scheduled_status_returns_empty_object ... ok
test scheduling_returns_the_scheduled_shape_and_publishes_later ... ok
test scheduling_validation_matches_mastodon ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

     Running tests/phase4_search_emoji.rs (target/debug/deps/phase4_search_emoji-9a58119bce0dc856)

running 3 tests
test search_requires_q_and_pages_with_mastodon_messages ... ok
test emoji_index_is_public_and_serves_images ... ok
test search_finds_accounts_hashtags_and_statuses ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

     Running tests/phase4_timelines.rs (target/debug/deps/phase4_timelines-b13576db7787f80e)

running 23 tests
test home_requires_a_user_token ... ok
test home_private_statuses_need_a_follow ... ok
test home_excludes_blocked_authors_in_both_directions ... ok
test home_direct_statuses_reach_only_the_mentioned_viewer ... ok
test home_show_reblogs_false_hides_that_follows_reblogs ... ok
test home_reply_rules_follow_the_parent ... ok
test home_orders_own_and_followed_statuses_newest_first ... ok
test home_timeline_carries_the_filtered_annotation ... ok
test home_own_statuses_always_appear_even_when_direct ... ok
test cursors_limit_and_link_headers_match_the_contract ... ok
test home_hides_muted_authors_and_reblogs_of_muted_targets ... ok
test tag_timeline_unknown_tag_is_an_empty_array_without_link ... ok
test cache_control_follows_the_feed_and_the_viewer ... ok
test domain_blocked_authors_are_excluded_for_the_viewer_only ... ok
test list_errors_are_exact ... ok
test list_replies_policy_list_keeps_replies_to_members ... ok
test public_local_flag_and_only_media_scope_the_feed ... ok
test public_excludes_blocked_blocked_by_and_muted_authors_for_the_viewer ... ok
test list_replies_policy_followed_and_none ... ok
test tag_timeline_normalizes_case_and_keeps_replies ... ok
test list_timeline_shows_member_statuses_only ... ok
test public_is_public_only_with_self_replies_and_no_reblogs ... ok
test sparse_filter_scan_stays_bounded ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s

     Running tests/runtime.rs (target/debug/deps/runtime-4f8d478edf59824c)

running 3 tests
test unknown_route_is_404 ... ok
test ready_reports_storage_available ... ok
test health_is_ok ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/secret_handling.rs (target/debug/deps/secret_handling-74f0497b97e57f79)

running 3 tests
test authorization_headers_are_redacted ... ok
test serialized_token_record_omits_plaintext ... ok
test database_never_contains_plaintext_secrets ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

   Doc-tests birchtree

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

==> scripts/smoke-phase1.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
==> start server
==> probes
==> graceful shutdown
==> CLI create-account
created account id=117457889311981568 username=smoke
==> restart preserves identity
SMOKE_OK
==> scripts/smoke-phase2.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
==> start server (registration open)
==> instance endpoints
==> register OAuth application
==> client-credentials token
==> app-only token is rejected on a user endpoint
==> register account (needs the client-credentials token)
==> verify_credentials
==> preferences
==> authorization-code + PKCE S256 flow
==> authorization code is single-use
==> revoke
==> restart with registration closed
==> graceful shutdown
SMOKE_PHASE2_OK
==> scripts/smoke-phase3.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
==> start server
==> setup: app + three accounts
==> create a status (alice)
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
SMOKE_PHASE3_OK
==> scripts/smoke-phase4.sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
==> start server
==> setup: app + two accounts
==> media: v1 upload (multipart) -> 200 with processed urls
==> media: serve the stored bytes
==> media: update description/focus
==> media: v2 upload of a video-ish file -> 202 while processing
==> media: rejected uploads
==> post a status with the attachment
==> follow + timelines
==> notifications + markers
==> scheduled status
==> search
==> custom emoji index (public, empty)
==> Vary header on API responses
SMOKE_PHASE4_OK
smoke-all: green
```
