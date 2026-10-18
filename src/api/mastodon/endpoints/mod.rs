//! Mastodon REST endpoint handlers.
//!
//! Each module owns one resource's routes. Handlers stay thin: they authenticate,
//! parse input, call one application use case, and serialize the result. No
//! business logic lives here.

pub mod accounts;
pub mod announcements;
pub mod apps;
pub mod emoji;
pub mod filters;
pub mod instance;
pub mod lists;
pub mod markers;
pub mod media;
pub mod notifications;
pub mod polls;
pub mod preferences;
pub mod relationships;
pub mod scheduled_statuses;
pub mod search;
pub mod statuses;
pub mod timelines;
pub mod suggestions;
pub mod donation_campaigns;
pub mod reports;
