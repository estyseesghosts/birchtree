# 2026-10-22 — S11 accounts + domain user (decomposition proposal)

## What changed
Application layer (`application/accounts.rs`, 1230 → ~1020 lines):
- `accounts_mfa.rs` (new): `request_confirmation_email`,
  `request_password_reset`, TOTP/enroll/confirm/revoke/verify,
  `LoginOutcome` + `login_with_mfa`, `INVALID_2FA_MESSAGE`
  (single `pub(crate) random_bytes`, deduped).
- `accounts_webauthn.rs` (new): begin/finish registration +
  authentication (imports `random_bytes` from `accounts_mfa`).
- **Single `register()` entry point**: new `RegisterInput` (+
  `InviteInput`/`PolicyInput`) and <80-line `register()` orchestrator
  uniting the three `register_account*` constructors in legacy order
  (profile → email gate → invite resolution → allowed-gate → create →
  redeem), so first-error precedence is unchanged. Legacy three become
  one-line shims; endpoint calls `register()` with both option groups.
  `create_registered()` holds the shared create core.
- Call sites updated to new paths (`auth.rs`, `oauth/authorization.rs`,
  2 test files). `InviteStore` stays in `accounts.rs` (merge-clean).
Domain layer (`domain/user.rs`, 1422 lines → dir, shim deleted):
- `roles.rs` (UserRole/AdminAction/PERM_*), `profile.rs` (time_zone/dob
  validators + `MINIMUM_AGE_YEARS`), `password.rs` (LocalUser/Argon2),
  `totp.rs`, `invite.rs`, `backup_codes.rs`, `challenge.rs`,
  `webauthn.rs` + `webauthn_cbor.rs` (split at 586 lines: CBOR/authdata
  parsers + sha256/verify moved out; shared `webauthn_err` +
  `WEBAUTHN_MAX_CEREMONY_BYTES` stay canonical in `webauthn.rs`).
  All files <400 lines; `mod.rs` re-exports preserve every
  `domain::user::*` path.
- `application/mod.rs` wiring for the S11 modules landed in the S4
  commit (shared file — carries the `media_*` lines too).

## Verification
- `cargo check --all-targets`: exit 0, zero new warnings.
- `cargo test` (full, incl. auth/MFA/WebAuthn vectors):
  1153 passed, 0 failed.
- `rustfmt --edition 2024 --check`: clean.
