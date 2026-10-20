# 2026-10-20 — remote-media cache plan (storage-cache.md review)

## Changed

- New `docs/remote-media-cache-plan.md` (Status: Draft): Birchtree-specific
  forward plan from `/root/storage-cache.md`. Records stubs
  (`src/media/remote_cache.rs`, `src/federation/remote_media.rs`), durable
  `FsBlobStore` foundation, design to freeze before Phase 6 (separate
  remote-cache store + metadata model, metadata-first ingest, on-demand
  default), shared policy (disabled/on_demand/warm, 512 MiB starting
  budget), build order, acceptance tests, and Pinetree-first sequencing
  dependency.
- `docs/README.md`: indexed the new plan.

## Verified

- Spot-checked cited files (`src/media/remote_cache.rs`,
  `src/federation/remote_media.rs`, `src/media/blob_store.rs`) before
  writing. No product code changed; no test run (docs-only session).

## Open

- Freeze the metadata schema and mirror key items into
  `federation-contract.md` / `persistence-conformance.md` and the Phase 6
  exit criteria when Phase 6 begins.
