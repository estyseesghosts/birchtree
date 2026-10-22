//! Wave 2 A5: remote downloads stage on the cache filesystem.
//!
//! The pooled handle stages under the cache root so `publish_tmp` is a
//! same-volume rename; bursty fetches never consume durable volume space.
//! Legacy rootless doubles keep the buffered path.

mod common;

use birchtree::application::ports::BlobStore;
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::fetch_stream;
use birchtree::media::pooled_blob_stores::PooledBlobStores;

#[test]
fn staging_root_is_the_cache_root_for_pooled_handles() {
    let dir = tempfile::tempdir().unwrap();
    let (durable, cache) =
        FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
            .expect("pools");
    let pools = PooledBlobStores::new(durable, cache);
    let staging = pools.staging_root().expect("staging root");
    assert_eq!(
        staging,
        dir.path().join("media_cache"),
        "pooled staging lives on the cache filesystem"
    );
    assert_ne!(
        pools.root_path().expect("durable root"),
        staging,
        "durable and staging roots differ"
    );
}

#[test]
fn single_pool_staging_is_its_own_root() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsBlobStore::open(dir.path().join("media")).expect("open");
    assert_eq!(
        store.staging_root().expect("staging"),
        store.root_path().expect("root"),
        "single-pool staging is a same-volume rename"
    );
}

#[test]
fn tmp_part_appears_under_cache_root_and_sweeper_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let (durable, cache) =
        FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
            .expect("pools");
    let pools = PooledBlobStores::new(durable, cache);
    let staging = pools.staging_root().expect("staging");
    let tmp = fetch_stream::tmp_path_for(&staging).expect("tmp");
    assert!(
        tmp.starts_with(&staging),
        "staging file lives under cache root: {}",
        tmp.display()
    );
    assert!(
        !tmp.starts_with(dir.path().join("media").as_path()) || tmp.starts_with(&staging),
        "never under the durable root alone"
    );
    // Leftover residue is swept from the cache root by the existing sweeper.
    std::fs::write(&tmp, b"residue").expect("write residue");
    let swept = fetch_stream::sweep_tmp(&staging).expect("sweep");
    assert!(swept >= 1, "residue swept from cache root");
    assert!(!tmp.exists(), "residue file gone");
}
