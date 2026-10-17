# Verification evidence — 2026-10-16 Phase 2

Append-only. Command output captured from the release-relevant baseline (the
Phase 2 working tree, before commit). Machine: Linux.

## `bash scripts/smoke-all.sh`

The full gate runs `cargo check --all-targets`, `cargo test`,
`scripts/smoke-phase1.sh`, and `scripts/smoke-phase2.sh`. Exit status `0`;
ends `smoke-all: green`.

```
==> cargo check --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.27s

==> cargo test
    Finished `test` profile [optimized + debuginfo] target(s) in 0.34s
running 45 tests
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/crash_recovery.rs
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/persistence_conformance.rs
running 13 tests
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/phase2_api.rs
running 13 tests
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/phase2_oauth.rs
running 13 tests
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/runtime.rs
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/secret_handling.rs
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

==> scripts/smoke-phase1.sh
==> start server
==> probes
==> graceful shutdown
==> CLI create-account
==> restart preserves identity
SMOKE_OK

==> scripts/smoke-phase2.sh
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
smoke-all: green
```

Totals: **93 passed, 0 failed** (45 unit + 3 + 13 + 13 + 13 + 3 + 3).

## `cargo test` — named Phase 2 cases

`phase2_oauth` (13):

- `app_registration_returns_credential_shape`
- `app_registration_requires_name_and_redirect_uri`
- `client_credentials_issues_app_only_token`
- `client_credentials_rejects_scope_escalation`
- `invalid_client_secret_is_rejected`
- `unsupported_grant_type_is_rejected`
- `authorization_code_pkce_flow_issues_user_token`
- `wrong_pkce_verifier_is_invalid_grant`
- `authorization_code_is_single_use`
- `redirect_uri_mismatch_is_invalid_grant`
- `revoke_invalidates_token`
- `revoke_with_wrong_client_is_forbidden`
- `revoke_empty_token_is_ok`

`phase2_api` (13):

- `registration_returns_user_token`
- `registration_closed_returns_forbidden`
- `registration_needs_client_credentials`
- `registration_needs_write_scope`
- `duplicate_username_is_rejected`
- `verify_credentials_returns_credential_shape`
- `account_show_is_public_and_404s_unknown`
- `update_credentials_persists_profile_and_settings`
- `preferences_require_a_user`
- `missing_token_is_unauthorized`
- `relationships_returns_relationship_shape`
- `instance_v1_shape`
- `instance_v2_shape_has_no_top_level_stats`

`persistence_conformance` (13; +3 this phase):

- `authorization_code_redeem_is_single_use`
- `authorization_code_redeem_is_atomic_under_contention`
- `registration_commits_confirmation_mail_job_atomically`

## `scripts/smoke-phase2.sh` — live curl transcript

Driven against the real binary (`target/debug/birchtree`) on a fresh data dir.
Client secret and tokens are real values from the run; bodies elided.

```
APP     = POST /api/v1/apps {client_name, redirect_uris, scopes}          -> 200
TOKEN   = POST /oauth/token grant_type=client_credentials                  -> 200
AUTHED  = GET  /api/v1/accounts/verify_credentials (app-only token)        -> 422
REG     = POST /api/v1/accounts (app-only token)                           -> 200 (user token)
VERIFY  = GET  /api/v1/accounts/verify_credentials (user token)            -> 200 username=smokey
PREFS   = GET  /api/v1/preferences                                         -> 200 visibility=public
AUTHORIZE= POST /oauth/authorize (login) then (decision=authorize)         -> 302 Location?code=...
EXCHANGE = POST /oauth/token grant_type=authorization_code + verifier      -> 200
REPLAY   = POST /oauth/token same code                                     -> 400 invalid_grant
REVOKE   = POST /oauth/revoke                                              -> 200 {}
REVOKED  = GET  /api/v1/preferences (revoked token)                        -> 401
CLOSED   = POST /api/v1/accounts after restart (closed)                    -> 403
```

## Notes

- The `phase2_oauth` PKCE flow is exercised end-to-end through the real router:
  login establishes the hashed session cookie, consent redirects with a
  one-time code, and the token endpoint redeems it with the RFC 7636 appendix-B
  verifier/challenge pair.
- Storage-level concurrency: eight threads exchanging the same authorization
  code yield exactly one winner (`authorization_code_redeem_is_atomic_under_contention`);
  the redeem-and-issue path is a single `redb` transaction.
- `cargo fmt`/`cargo clippy` were not available in the environment and are not
  part of CI; this is an explicit gap, not a passing check.
