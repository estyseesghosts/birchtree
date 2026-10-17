# docs/streaming-contract.md

Status: Draft

Streaming contract (per `birchtree.md` §2.6 and §6).

## Transports

SSE and WebSocket hosted in-process in Rust (no separate Node streaming service). Stream hostname discovery/redirect behavior is observable API behavior; implemented per the pinned controller contract.

## Event envelopes

Status `update`/`delete`, `status.update`, `notification`, plus stream-specific events/payloads in the declared profile.

## Streams

user, user notifications, public, public:local, public:media, hashtag, list, and others in the profile. Token types/scopes, parameters, filtering, blocks, mutes, status visibility all apply.

## Behavior

Heartbeats, connection/subscription bounds, cleanup, disconnect/reconnect, backpressure. Streaming is a consumer of durable state/jobs, not the durable event log; reconnect does not imply persisted events for offline clients unless an endpoint provides that.

Auth/scope rules, visibility gating, and no cross-user/private leakage under concurrent follow/block/unfollow/status-delete changes are tested.
