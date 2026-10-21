pub mod ingest;
pub mod blob_store;
pub mod processing;
pub mod remote_cache;
pub mod transcode;
pub mod fetch_stream;
pub mod reconciler;

// Agent A: single canonical re-export for the two-pool handles. Agents B/C
// add `fetch_stream` / `reconciler` as their own `pub mod` lines in their
// files; nothing is pre-declared here so this list always compiles.
pub use blob_store::FsBlobStore;
