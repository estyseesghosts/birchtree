//! Federation adapter (Phase 6): the address and URL policy, the safe outbound
//! HTTP client, and the federation job handlers (events, delivery, threads,
//! remote media). Protocol shapes live in `activitypub/`; business rules live
//! in `application/`.
pub mod delivery;
pub mod events;
pub mod media;
pub mod net;
pub mod policy;
pub mod preview;
pub mod threads;
pub mod tombstone;
pub mod resolver;
pub mod retry;
pub mod dedup;
pub mod remote_media;
