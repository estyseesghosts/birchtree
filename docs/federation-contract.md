# docs/federation-contract.md

Status: Draft

ActivityPub federation contract (per `birchtree.md` §2.7 and §6). Federation is delegated to the `activitypub-federation-rust` crate; this document describes the boundary between birchtree's application layer and that crate, not a hand-rolled ActivityPub implementation.

## Ingress (bounded protocol adapter)

1. Authenticate HTTP signature + digest.
2. Parse and validate untrusted JSON-LD/ActivityStreams (size limits, content validation).
3. Deduplicate activity IDs (canonical activity URI dedup record).
4. Apply exactly one application command.
5. Persist follow-up jobs in the same transaction.
6. Return the appropriate acknowledgement.

Inbound failures never roll back committed local actions.

## Egress (durable per-destination delivery)

- Commit delivery jobs before delivery.
- Sign the correct request (HTTP signature), correct digest.
- Retry per destination with backoff/cooldown; detect permanent failures; terminal quarantine.
- One failing host must not block unrelated delivery.
- Safe HTTP client: DNS/IP SSRF defenses, redirect/time/size limits, content validation.

## Initial activity set (Phase 6)

Create/Note, Update, Delete, Follow/Accept/Reject/Undo, Like/Undo, Announce/Undo, Block, quote Request/Accept/Reject/Revoke. `Flag`/report if the profile includes it.

## Discovery

WebFinger `acct:` resolution, NodeInfo, actor and key documents, inbox/shared inbox/outbox, object dereferencing, canonical URI mapping.

## Non-goals

No outbound HTTP inside a local status/follow transaction. No multi-node mode until storage adapter + media arrangement + routing + shared conformance suite are implemented and tested.
