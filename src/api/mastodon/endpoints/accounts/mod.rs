//! Account endpoints: registration, profile read/update, credentials and
//! relationships.
//!
//! Contracts follow Mastodon v4.7.3: `POST /api/v1/accounts` requires an
//! app-only (client-credentials) token with `write`/`write:accounts`; read
//! endpoints accept `read`/`read:accounts`; `verify_credentials` and
//! `update_credentials` require a user token.

pub mod credentials;
pub mod directory;
pub mod mail_reset;
pub mod profile;
pub mod profile_images;
pub mod read;
pub mod register;
pub mod relations;
pub mod support;

pub use credentials::update_credentials;
pub use directory::{index, lookup, search};
pub use mail_reset::{
    PasswordResetQuery, email_confirmation_check, email_confirmation_create, invite_show,
    password_reset_create, password_reset_page, password_reset_update, reset_bearer_live,
    reset_bearer_ttl_ms, validate_reset_password,
};
pub use profile::{
    profile_avatar_destroy, profile_header_destroy, profile_show, profile_update,
};
pub use read::{identity_proofs_show, show, verify_credentials};
pub use register::register;
pub use relations::relationships;
