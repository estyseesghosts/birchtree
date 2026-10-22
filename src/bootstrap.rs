//! Serve-time bootstrap stages extracted verbatim from `main.rs::serve`.
//!
//! S3 slice group B (`birchtree-decomposition-proposal.md` S3 step 2).
//! Each function below keeps the same body as the inline block in `serve`:
//! no behavior change, no config key renames, no new dependencies. All
//! item paths stay absolute (`birchtree::...`) as in `main.rs`. The
//! orchestration (`serve` calling these in order) stays in `main.rs` and is
//! owned by the parent slice; this file only hosts the extracted stages.

use std::sync::Arc;

/// Load the secret keyring, exiting the process on fatal misconfiguration.
///
/// Adapted from `main.rs::load_secret_box` (lines 430-443) plus the
/// exit-on-error match in `serve` (lines 472-488). The `Result`-returning
/// helper cannot be reused directly from here, so its two keyring attempts
/// run inline in the closure below with the exact same error strings; the
/// `match` on the outcome is verbatim from `serve`.
/// SecretBox type path used in `main.rs`: line 432 spells
/// `birchtree::persistence::redb::secrets::SecretBox` in the signature while
/// line 433 imports it (`use ...::SecretBox;`) for the bare `SecretBox::`
/// uses at lines 435, 439 and 482. This file uses the absolute path
/// everywhere.
pub(crate) fn load_secret_box_or_exit(
    cfg: &birchtree::config::Config,
) -> birchtree::persistence::redb::secrets::SecretBox {
    let loaded: Result<birchtree::persistence::redb::secrets::SecretBox, String> = (|| {
        if let Some(path) = &cfg.secret_keyring_file {
            return birchtree::persistence::redb::secrets::SecretBox::from_keyring_file(path)
                .map_err(|err| format!("secret keyring file {}: {err}", path.display()));
        }
        if let Some(variable) = &cfg.secret_keyring_env {
            return birchtree::persistence::redb::secrets::SecretBox::from_keyring_env(variable)
                .map_err(|err| format!("secret keyring env {variable}: {err}"));
        }
        Err("no secret keyring configured (secret_keyring_file or secret_keyring_env)".to_string())
    })();
    // Secrets first: TOTP seeds, local signing keys and mail-outbox bearers
    // seal under this keyring. Production refuses to start without one; lab
    // runs keep long-lived secrets in plaintext with a warning.
    match loaded {
        Ok(secret_box) => secret_box,
        Err(err) => {
            if cfg.mode == birchtree::config::RuntimeMode::Lab
                || cfg.unsafe_production_override_active()
            {
                tracing::warn!(
                    error = %err,
                    "starting without a secret keyring: secrets stay in plaintext (lab only)"
                );
                birchtree::persistence::redb::secrets::SecretBox::disabled()
            } else {
                eprintln!("secret keyring: {err}; refusing to start in production mode");
                std::process::exit(1);
            }
        }
    }
}

/// Validate the storage layout and open the durable + cache blob pools.
///
/// Verbatim from `serve` (lines 512-539), minus the final
/// `Arc<dyn BlobStore>` wrapper at line 539, which stays with the caller:
/// the returned triple is exactly `(durable_blobs, cache_blobs, pools)` as
/// bound and built inline in `serve`.
pub(crate) fn open_blob_pools(
    cfg: &birchtree::config::Config,
) -> (
    birchtree::media::blob_store::FsBlobStore,
    birchtree::media::blob_store::FsBlobStore,
    birchtree::media::pooled_blob_stores::PooledBlobStores,
) {
    // Physical layout before anything is opened: the durable pool (local
    // uploads, never evicted) and the cache pool (remote bytes, evictable)
    // must be disjoint trees that exclude the database file, resolved
    // through symlinks. String comparison is not enough.
    if let Err(err) = birchtree::media::storage_roots::validate_layout(
        &cfg.durable_dir_resolved(),
        &cfg.cache_dir_resolved(),
        &cfg.database_path(),
    ) {
        eprintln!("storage layout: {err}");
        std::process::exit(1);
    }
    let (durable_blobs, cache_blobs) = birchtree::media::blob_store::FsBlobStore::open_pair(
        cfg.durable_dir_resolved(),
        cfg.cache_dir_resolved(),
    )
    .unwrap_or_else(|err| {
        eprintln!("open blob stores: {err}");
        std::process::exit(1);
    });
    // One routing handle: local keys land in the durable pool, `remote/…`
    // keys in the cache pool (pre-split durable `remote/` bytes stay
    // readable).
    let pools = birchtree::media::pooled_blob_stores::PooledBlobStores::new(
        durable_blobs.clone(),
        cache_blobs.clone(),
    );
    (durable_blobs, cache_blobs, pools)
}

/// Configure the process-wide cache admission ledger, build the cache
/// lifecycle service, and run the pre-traffic restore + cap enforcement.
///
/// Verbatim from `serve` (lines 540-589): the ledger configuration, the
/// `CacheMaintenance` construction with the durable residue sweep, the
/// startup restore, the over-cap eviction, and the maintenance seed.
/// Returns the service handle the worker later embeds (`worker_extra`
/// in `serve`).
pub(crate) fn restore_cache_ledger(
    store: &Arc<birchtree::persistence::redb::RedbStore>,
    cache_blobs: &birchtree::media::blob_store::FsBlobStore,
    durable_blobs: &birchtree::media::blob_store::FsBlobStore,
    cfg: &birchtree::config::Config,
    clock: &Arc<dyn birchtree::domain::Clock>,
) -> Arc<birchtree::media::cache_maintenance::CacheMaintenance> {
    // Storage overhaul (Agent B): install the operator budgets into the
    // process-wide cache admission ledger (in-flight reservations gate the
    // fetch path before any byte is read).
    birchtree::media::remote_cache::configure_process_ledger(cfg.remote_cache.clone());
    // The cache lifecycle service owns eviction + reconciliation. Restore
    // rehydrates the ledger from the durable rows and the cache pool before
    // traffic is accepted; the seed starts the bounded maintenance chains.
    let maint = Arc::new(
        birchtree::media::cache_maintenance::CacheMaintenance::new(
            store.clone(),
            cache_blobs.clone(),
            cfg.remote_cache.clone(),
        )
        .with_durable_residue_sweep(durable_blobs.clone()),
    );
    match maint.restore_at_startup(clock.now_ms()) {
        Ok(report) => tracing::info!(
            adopted = report.adopted,
            missing = report.missing,
            residue_removed = report.residue_removed,
            "cache restore at startup"
        ),
        Err(err) => {
            eprintln!("cache restore at startup: {err}");
            std::process::exit(1);
        }
    }
    // Over-cap after restore (or after a shadow migration that adopted more
    // bytes than the caps allow) is evicted with the same durable-first
    // primitive as admission and maintenance, before the listener binds:
    // redb and disk agree before traffic is accepted.
    match maint.enforce_caps_at_startup(clock.now_ms()) {
        Ok(report) => {
            if report.pressure > 0 || report.expired > 0 {
                tracing::info!(
                    expired = report.expired,
                    pressure = report.pressure,
                    blobs_deleted = report.blobs_deleted,
                    "cache startup cap enforcement"
                );
            }
        }
        Err(err) => {
            eprintln!("cache startup cap enforcement: {err}");
            std::process::exit(1);
        }
    }
    if let Err(err) = maint.seed_jobs(&**store, clock.now_ms()) {
        tracing::warn!(error = %err, "maintenance seed failed");
    }
    maint
}

/// Build the shared HTTP application state: search/tag backfill seeds,
/// streaming hub, request context, federation config, and `ApiState`.
///
/// Verbatim from `serve` (lines 590-698). Async because the instance-actor
/// lookup and the federation config build await. Ends with the `ApiState`
/// literal; the readiness probes (lines 699-802) and the router build
/// (line 803) stay in `serve`, so this returns `ApiState` only and the
/// caller reuses `state.context`, `state.streams` and `state.federation`
/// for the worker and router.
pub(crate) async fn build_app_state(
    cfg: &birchtree::config::Config,
    store: Arc<birchtree::persistence::redb::RedbStore>,
    clock: Arc<dyn birchtree::domain::Clock>,
    ids: Arc<birchtree::domain::id::TimestampIdGenerator>,
    blobs: Arc<dyn birchtree::application::ports::BlobStore>,
) -> birchtree::api::mastodon::ApiState {
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(cfg));
    // Phase 7.2/7.3 startup seeds: the search index is in-memory derived
    // state, so a restart re-seeds its backfill chain when empty; the tag
    // backfill re-scans until a pass plans nothing (idempotent, bounded).
    // Seed failures only degrade reads until the next restart.
    {
        use birchtree::application::ports::JobQueue;
        use birchtree::domain::JobKind;
        if birchtree::search::global().is_empty()
            && cfg.search_mode == birchtree::search::SearchMode::Embedded
        {
            if let Err(err) = store.enqueue(
                JobKind::SearchBackfill,
                birchtree::jobs::search_index::backfill_payload(
                    None,
                    birchtree::search::BACKFILL_DEFAULT_BATCH,
                ),
                clock.now_ms(),
            ) {
                tracing::warn!(error = %err, "search backfill seed failed");
            }
        }
        if let Err(err) = store.enqueue(
            JobKind::TagBackfill,
            birchtree::jobs::search_index::backfill_payload(
                None,
                birchtree::jobs::tag_backfill::BACKFILL_BATCH_CAP,
            ),
            clock.now_ms(),
        ) {
            tracing::warn!(error = %err, "tag backfill seed failed");
        }
    }
    let streams = birchtree::streaming::Hub::new(
        store.clone(),
        clock.clone(),
        server.clone(),
        birchtree::streaming::Limits::default(),
    );
    let streaming: Arc<dyn birchtree::application::ports::StreamingSink> =
        Arc::new(streams.clone());
    let context = Arc::new(birchtree::application::context::Context::new(
        clock.clone(),
        ids,
        store.clone(),
        blobs.clone(),
        streaming.clone(),
    ));
    // Federation (Phase 6). Lab mode permits plain-HTTP targets and
    // private-network allowlists; production refuses them in
    // `check_runtime_mode` unless the explicit override is set (see the
    // startup summary logged in `main`).
    let lab = cfg.mode == birchtree::config::RuntimeMode::Lab;
    let extra_roots = match &cfg.federation_extra_ca {
        Some(path) => {
            let pem = std::fs::read(path).unwrap_or_else(|err| {
                eprintln!("read --federation-extra-ca {}: {err}", path.display());
                std::process::exit(2);
            });
            reqwest::Certificate::from_pem_bundle(&pem).unwrap_or_else(|err| {
                eprintln!("parse --federation-extra-ca {}: {err}", path.display());
                std::process::exit(2);
            })
        }
        None => Vec::new(),
    };
    let http_policy = birchtree::federation::net::SafeHttpConfig {
        allow_http: cfg.federation_allow_http,
        allowlist: cfg.federation_allowlist.clone(),
        extra_roots,
        ..Default::default()
    };
    let instance = birchtree::activitypub::instance::ensure(&*store, &cfg.normalized_public_url())
        .await
        .unwrap_or_else(|err| {
            eprintln!("instance actor: {err}");
            std::process::exit(1);
        });
    let federation = birchtree::activitypub::build_config_with_remote_cache(
        context.clone(),
        &cfg.normalized_public_url(),
        cfg.registration_open,
        &cfg.site_title,
        &cfg.site_description,
        &http_policy,
        lab,
        Some(&instance),
        cfg.federation_authorized_fetch,
        &cfg.relay_inboxes,
        &cfg.remote_cache,
        &cfg.license_policy,
        &cfg.place_policy,
    )
    .await
    .unwrap_or_else(|err| {
        eprintln!("start federation: {err}");
        std::process::exit(1);
    });
    birchtree::api::mastodon::ApiState {
        context,
        trusted_proxies: Arc::new(cfg.trusted_proxies.clone()),
        max_body_size: cfg.max_body_size,
        server,
        streams,
        federation: federation.clone(),
        rate_limit_multiplier: cfg.rate_limit_multiplier,
        mail_env: Arc::new(birchtree::mailer::MailEnv::from_config(cfg)),
        invites: store.clone() as std::sync::Arc<dyn birchtree::application::accounts::InviteStore>,
    }
}
