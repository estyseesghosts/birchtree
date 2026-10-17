# Verification evidence — 2026-10-16 Phase 1

Append-only. Command output captured from the release-relevant baseline
(`git` working tree of Phase 1, before commit). Machine: Linux (CI target).

## `bash scripts/smoke-all.sh`

The full gate runs `cargo check --all-targets`, `cargo test`, and
`scripts/smoke-phase1.sh`.

```
==> cargo check --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s)

==> cargo test
...
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/crash_recovery.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/persistence_conformance.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/runtime.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/secret_handling.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

==> scripts/smoke-phase1.sh
==> probes
==> graceful shutdown
    INFO birchtree: shutdown signal received
    INFO birchtree::jobs::worker: job worker stopped
    INFO birchtree: birchtree stopped
==> CLI create-account
created account id=117453460462108672 username=smoke
==> restart preserves identity
    INFO birchtree::jobs::handlers: user_created follow-up job=117453460462108674 account_id=117453460462108672
SMOKE_OK
smoke-all: green
```

## `cargo test` — named cases

Unit tests (29) include: `application::crypto::tests::hashes_are_salted`,
`domain::id` ordering/serde tests, config validation
(`rejects_world_writable_data_dir`), and `persistence::redb` store probes.

`crash_recovery` (3):

- `aborted_open_leaves_no_partial_state`
- `committed_account_and_job_survive_sigkill`
- `cli_create_account_persists_and_is_readable`

`persistence_conformance` (10):

- `create_and_read_back_account_and_credential`
- `unique_handle_race_has_exactly_one_winner`
- `duplicate_handle_is_rejected_case_insensitively`
- `account_creation_commits_one_follow_up_job_atomically`
- `job_claim_complete_retry_and_quarantine`
- `oauth_app_and_token_lifecycle`
- `login_session_lifecycle_and_expiry`
- `password_authentication`
- `crypto_helpers_reject_tampered_hash`
- `migrations_and_data_survive_restart`

`runtime` (3): `health_is_ok`, `ready_reports_storage_available`,
`unknown_route_is_404`.

`secret_handling` (3): `authorization_headers_are_redacted`,
`database_never_contains_plaintext_secrets`,
`serialized_token_record_omits_plaintext`.

## Notes

- Crash tests use the test-only `crash-harness --mode=...` subcommand and a real
  `SIGKILL`, not a simulated failure, to prove transaction atomicity.
- `cargo fmt`/`cargo clippy` were not available in the environment and are not
  part of CI; they are an explicit gap, not a passing check.
