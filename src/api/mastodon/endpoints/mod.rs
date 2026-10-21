//! Mastodon REST endpoint handlers.
//!
//! Each module owns one resource's routes. Handlers stay thin: they authenticate,
//! parse input, call one application use case, and serialize the result. No
//! business logic lives here.

pub mod accounts;
pub mod admin_accounts;
pub mod admin_infra;
pub mod admin_reports;
pub mod admin_trends;
pub mod announcements;
pub mod annual_reports;
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
pub mod suggestions;
pub mod directory;
pub mod timelines;
pub mod trends;
pub mod donation_campaigns;
pub mod reports;
pub mod conversations;
pub mod push;
pub mod relations_extra;
pub mod timelines_link;
pub mod web;
// Gaps batch (`docs/gaps.md` Step 0): G-C owns this module.
pub mod collections;
