//! Mastodon REST endpoint handlers.
//!
//! Each module owns one resource's routes. Handlers stay thin: they authenticate,
//! parse input, call one application use case, and serialize the result. No
//! business logic lives here.

pub mod accounts;
pub mod apps;
pub mod instance;
pub mod preferences;
