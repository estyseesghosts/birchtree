//! Federation adapter (Phase 6): the address and URL policy, the safe outbound
//! HTTP client, and the federation job handlers (events, delivery, threads,
//! remote media). Protocol shapes live in `activitypub/`; business rules live
//! in `application/`.
pub mod dedup;
pub mod delivery;
pub mod delivery_send;
pub mod events;
pub mod events_build;
pub mod media;
pub mod media_fetch;
pub mod net;
pub mod policy;
pub mod preview;
pub mod remote_media;
pub mod resolver;
pub mod retry;
pub mod threads;
pub mod threads_auth;
pub mod threads_auth_stages;
pub mod tombstone;
