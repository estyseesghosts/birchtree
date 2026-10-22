use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use birchtree::api::mastodon::router::build_router_with_probes;
use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::ports::MailSender;
use birchtree::config::Config;
use birchtree::domain::id::{SystemClock, TimestampIdGenerator};
use birchtree::domain::{Clock, IdGenerator};
use birchtree::jobs;
use birchtree::mail::LoggingMailSender;
use birchtree::observability;
use birchtree::persistence::redb::RedbStore;

mod bootstrap;
mod cli;

use cli::{Args, parse_args};

fn build_store(cfg: &Config) -> (Arc<RedbStore>, Arc<dyn Clock>, Arc<TimestampIdGenerator>) {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let store =
        RedbStore::open(cfg.database_path(), clock.clone(), ids.clone()).unwrap_or_else(|err| {
            eprintln!("open storage: {err}");
            std::process::exit(1);
        });
    // ID seeding: the generator's last value lives in process memory, so a
    // restart in the same millisecond, a clock rollback or a restored older
    // database must not reset it behind the highest persisted ID (primary-key
    // collisions would overwrite rows). A large skew means a restored backup
    // or a rolled-back clock: keep counting above it and warn.
    match store.highest_persisted_id() {
        Ok(highest) => {
            let skew_ms = ids.observe(highest);
            if skew_ms >= birchtree::domain::id::SKEW_WARN_THRESHOLD_MS {
                tracing::warn!(
                    skew_ms,
                    highest = highest.value(),
                    "persisted IDs run ahead of the wall clock (restored database or clock rollback); issuing IDs above them"
                );
            }
        }
        Err(err) => {
            eprintln!("read highest persisted id: {err}");
            std::process::exit(1);
        }
    }
    (Arc::new(store), clock, ids)
}

/// `create-account --username=<name> --password=<pw>`: create a local account
/// from the CLI for operators (the Phase 1 bootstrap path). With
/// `--role=admin|moderator` the new account also gets that staff role for the
/// admin API (Phase 7.3); the default is no role.
fn run_create_account(cfg: &Config, args: &Args) -> ! {
    let username = args.username.clone().unwrap_or_else(|| {
        eprintln!("create-account requires --username=<name>");
        std::process::exit(2);
    });
    let password = args
        .password
        .clone()
        .or_else(|| std::env::var("BIRCHTREE_PASSWORD").ok())
        .unwrap_or_else(|| {
            eprintln!("create-account requires --password=<pw> (or $BIRCHTREE_PASSWORD)");
            std::process::exit(2);
        });
    let role = args.role.as_deref().map(|s| {
        birchtree::domain::UserRole::parse_cli(s).unwrap_or_else(|| {
            eprintln!("invalid --role=<admin|moderator> (got {s:?})");
            std::process::exit(2);
        })
    });

    let (store, clock, _ids) = build_store(cfg);
    let cmd = CreateAccountCommand {
        username,
        email: args.email.clone(),
        password,
        display_name: args.display_name.clone(),
    };
    match create_account(&*store, &cfg.normalized_public_url(), clock.now_ms(), cmd) {
        Ok(account) => {
            use birchtree::application::ports::AccountStore;
            if let Some(role) = role.filter(|r| r.is_staff()) {
                if let Err(err) = store.set_user_role(account.id, role) {
                    eprintln!("create-account granted nothing: set role failed: {err}");
                    std::process::exit(1);
                }
            }
            // IDs are decimal strings on the wire.
            println!(
                "created account id={} username={}",
                account.id, account.username
            );
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("create-account failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `grant-role --username=<name> --role=<admin|moderator|none>`: grant (or
/// clear) a staff role on an existing local account (Phase 7.3). Operators use
/// this to promote accounts created before roles existed.
fn run_grant_role(cfg: &Config, args: &Args) -> ! {
    use birchtree::application::ports::AccountStore;

    let username = args.username.clone().unwrap_or_else(|| {
        eprintln!("grant-role requires --username=<name>");
        std::process::exit(2);
    });
    let role = args
        .role
        .as_deref()
        .map(|s| {
            birchtree::domain::UserRole::parse_cli(s).unwrap_or_else(|| {
                eprintln!("grant-role requires --role=<admin|moderator|none> (got {s:?})");
                std::process::exit(2);
            })
        })
        .unwrap_or_else(|| {
            eprintln!("grant-role requires --role=<admin|moderator|none>");
            std::process::exit(2);
        });

    let (store, _clock, _ids) = build_store(cfg);
    let base_url = cfg.normalized_public_url();
    let domain = create_account_domain(&base_url);
    let normalized = birchtree::domain::account::normalize_username(&username);
    let account = match store.account_by_handle(&normalized, &domain) {
        Ok(Some(account)) => account,
        Ok(None) => {
            eprintln!("grant-role failed: no account named {username:?}");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("grant-role failed: {err}");
            std::process::exit(1);
        }
    };
    match store.set_user_role(account.id, role) {
        Ok(_) => {
            let label = match role {
                birchtree::domain::UserRole::Admin => "admin",
                birchtree::domain::UserRole::Moderator => "moderator",
                birchtree::domain::UserRole::None => "none",
            };
            println!(
                "granted role={label} account id={} username={}",
                account.id, account.username
            );
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("grant-role failed: {err}");
            std::process::exit(1);
        }
    }
}

/// Bare host of the public base URL (the local account domain).
fn create_account_domain(base_url: &str) -> String {
    birchtree::application::accounts::host_from_url(base_url)
}

/// Test-only harness used by `tests/crash_recovery.rs` to exercise `kill -9`
/// durability of committed mutations and their jobs.
fn run_crash_harness(cfg: &Config, args: &Args) -> ! {
    let mode = args.harness_mode.clone().unwrap_or_else(|| {
        eprintln!("crash-harness requires --mode=open-and-spin|commit-and-spin");
        std::process::exit(2);
    });
    let (store, _clock, _ids) = build_store(cfg);
    match mode.as_str() {
        "open-and-spin" => {
            println!("OPENED");
            std::io::stdout().flush().ok();
        }
        "commit-and-spin" => {
            let username = args
                .username
                .clone()
                .unwrap_or_else(|| "crashuser".to_string());
            let cmd = CreateAccountCommand {
                username,
                email: None,
                password: "crashpassword".to_string(),
                display_name: None,
            };
            let account = create_account(
                &*store,
                &cfg.normalized_public_url(),
                SystemClock.now_ms(),
                cmd,
            )
            .unwrap_or_else(|err| {
                eprintln!("commit failed: {err}");
                std::process::exit(1);
            });
            println!("COMMITTED {}", account.id);
            std::io::stdout().flush().ok();
        }
        other => {
            eprintln!("unknown crash-harness mode: {other}");
            std::process::exit(2);
        }
    }
    // Spin so the parent can SIGKILL the process at a controlled point.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

async fn serve(cfg: Config) {
    let (store, clock, ids) = build_store(&cfg);
    // Secrets first: TOTP seeds, local signing keys and mail-outbox bearers
    // seal under this keyring. Production refuses to start without one; lab
    // runs keep long-lived secrets in plaintext with a warning.
    let secret_box = bootstrap::load_secret_box_or_exit(&cfg);
    store.install_secret_box(secret_box.clone());
    if secret_box.is_enabled() {
        match store.seal_legacy_secrets() {
            Ok(report) => tracing::info!(totp_rows = report.totp_rows, "sealed legacy secrets"),
            Err(err) => {
                if birchtree::mailer::seal_failure_is_fatal(cfg.mode) {
                    eprintln!("legacy secret sealing failed: {err}; refusing to start");
                    std::process::exit(1);
                }
                tracing::warn!(error = %err, "legacy secret sealing failed");
            }
        }
        match store.seal_legacy_actor_keys() {
            Ok(rows) => tracing::info!(actor_key_rows = rows, "sealed legacy actor keys"),
            Err(err) => {
                if birchtree::mailer::seal_failure_is_fatal(cfg.mode) {
                    eprintln!("legacy actor key sealing failed: {err}; refusing to start");
                    std::process::exit(1);
                }
                tracing::warn!(error = %err, "legacy actor key sealing failed");
            }
        }
    }
    let (durable_blobs, cache_blobs, pools) = bootstrap::open_blob_pools(&cfg);
    let blobs: Arc<dyn birchtree::application::ports::BlobStore> = Arc::new(pools);
    let maint = bootstrap::restore_cache_ledger(&store, &cache_blobs, &durable_blobs, &cfg, &clock);
    let state =
        bootstrap::build_app_state(&cfg, store.clone(), clock.clone(), ids, blobs.clone()).await;
    // Readiness + metrics: storage writability, worker progress, pool
    // writability, queue health, cache accounting and media latency. The
    // heartbeat is beaten every worker loop iteration (idle or busy).
    let heartbeat = Arc::new(birchtree::api::mastodon::router::readiness::WorkerHeartbeat::new());
    let probes = {
        use birchtree::api::mastodon::router::readiness::{
            RuntimeProbes, writable_directory_check,
        };
        let store = store.clone();
        let queue_store = store.clone();
        let queue_age_store = store.clone();
        let queue_retry_store = store.clone();
        let queue_quarantine_store = store.clone();
        let queue_maint_store = store.clone();
        let cache_store = store.clone();
        let cache_inflight_store = store.clone();
        let write_store = store.clone();
        let beat = heartbeat.clone();
        let beat_age = heartbeat.clone();
        let clock_now = clock.clone();
        RuntimeProbes::new()
            .with_check("storage_write", move || write_store.check_write())
            .with_check("worker_progress", move || {
                beat.is_fresh(std::time::Duration::from_secs(30))
            })
            .with_check(
                "durable_pool",
                writable_directory_check(cfg.durable_dir_resolved()),
            )
            .with_check(
                "cache_pool",
                writable_directory_check(cfg.cache_dir_resolved()),
            )
            .with_gauge("queue_backlog", move || {
                queue_store
                    .all_jobs()
                    .map(|jobs| jobs.iter().filter(|job| !job.state.is_terminal()).count() as u64)
                    .unwrap_or(0)
            })
            .with_gauge("queue_oldest_age_ms", move || {
                let now = clock_now.now_ms();
                queue_age_store
                    .all_jobs()
                    .map(|jobs| {
                        jobs.iter()
                            .filter(|job| !job.state.is_terminal())
                            .map(|job| now.saturating_sub(job.created_at_ms))
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0)
            })
            .with_gauge("queue_retried", move || {
                queue_retry_store
                    .all_jobs()
                    .map(|jobs| jobs.iter().filter(|job| job.attempts > 0).count() as u64)
                    .unwrap_or(0)
            })
            .with_gauge("queue_quarantined", move || {
                queue_quarantine_store
                    .all_jobs()
                    .map(|jobs| {
                        jobs.iter()
                            .filter(|job| job.state == birchtree::domain::JobState::Quarantined)
                            .count() as u64
                    })
                    .unwrap_or(0)
            })
            .with_gauge("queue_maintenance_pending", move || {
                queue_maint_store
                    .all_jobs()
                    .map(|jobs| {
                        jobs.iter()
                            .filter(|job| {
                                !job.state.is_terminal()
                                    && matches!(
                                        job.kind,
                                        birchtree::domain::JobKind::CacheEvict
                                            | birchtree::domain::JobKind::ReconcileStorage
                                    )
                            })
                            .count() as u64
                    })
                    .unwrap_or(0)
            })
            .with_gauge("cache_stored_bytes", move || {
                use birchtree::application::ports::CacheStore;
                cache_store
                    .cache_accounting()
                    .map(|usage| usage.stored_total)
                    .unwrap_or(0)
            })
            .with_gauge("cache_inflight_bytes", move || {
                use birchtree::application::ports::CacheStore;
                cache_inflight_store
                    .cache_accounting()
                    .map(|usage| usage.inflight_total)
                    .unwrap_or(0)
            })
            .with_gauge("media_job_latency_ms", || {
                birchtree::jobs::worker::last_media_job_latency_ms()
            })
            .with_gauge("worker_heartbeat_age_ms", move || beat_age.age_ms())
    };
    let streaming: Arc<dyn birchtree::application::ports::StreamingSink> =
        Arc::new(state.streams.clone());
    let federation = state.federation.clone();
    let app = build_router_with_probes(state, probes);
    // Outbound mail: real SMTP delivery through the worker when configured,
    // the explicit logging adapter otherwise (registration jobs then log
    // instead of sending; resend endpoints answer 503).
    let mailer: Arc<dyn MailSender> = if cfg.mail_enabled() {
        let from = cfg.mail_from.clone().unwrap_or_default();
        let backend = birchtree::mailer::mailer_from_config(
            cfg.smtp_url.as_deref(),
            cfg.mail_from.as_deref(),
        )
        .unwrap_or_else(|err| {
            eprintln!("configuration error: {err}");
            std::process::exit(1);
        });
        Arc::new(birchtree::mail::SmtpJobSender::new(
            backend,
            &from,
            &cfg.normalized_public_url(),
        ))
    } else {
        Arc::new(LoggingMailSender)
    };

    let listener = {
        // Fail closed when signup could strand confirmation mail (open
        // registrations or enabled invites with no SMTP provider): never
        // bind a server that mints unconfirmable accounts. Lab passes with
        // a warning (the mail job stays explicitly undelivered).
        if let Err(err) = birchtree::mailer::require_mail_for_open_registration(&cfg) {
            eprintln!("configuration error: {err}");
            std::process::exit(1);
        }
        if birchtree::mailer::signup_without_mail(&cfg) {
            tracing::warn!(
                "signups are possible without an SMTP provider; confirmation mail will not deliver"
            );
        }
        tokio::net::TcpListener::bind(cfg.listen)
            .await
            .unwrap_or_else(|err| {
                eprintln!("bind {}: {err}", cfg.listen);
                std::process::exit(1);
            })
    };

    tracing::info!(
        listen = %cfg.listen,
        public_url = %cfg.normalized_public_url(),
        database = %cfg.database_path().display(),
        "birchtree listening"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Phase 7 worker resources: push delivery identity + ffmpeg toolchain.
    // A missing/non-working ffmpeg with transcoding on is a startup error,
    // never a silent passthrough (operator decision, Phase 7.1).
    let transcode = cfg.transcode_config().unwrap_or_else(|err| {
        eprintln!("configuration error: {err}");
        std::process::exit(1);
    });
    // A `SIGKILL` between ffmpeg spawn and temp-guard drop orphans temp
    // files permanently; sweep them once at startup (bounded, age-gated, so
    // a live job's fresh files are never touched).
    let swept = birchtree::media::transcode::sweep_tmp_files(
        &std::env::temp_dir(),
        birchtree::media::transcode::TMP_SWEEP_GRACE_MS,
        birchtree::media::transcode::TMP_SWEEP_CAP,
    );
    if swept > 0 {
        tracing::info!(swept, "removed stale transcode temp files at startup");
    }
    let worker_extra = jobs::worker::WorkerExtra {
        push_client: birchtree::application::push_client::production_push_client().unwrap_or_else(
            |err| {
                eprintln!("build push http client: {err}");
                std::process::exit(1);
            },
        ),
        vapid: birchtree::application::push::VapidKeys {
            public_b64: cfg.vapid_public_key.clone(),
            private_b64: cfg.vapid_private_key.clone(),
        },
        push_subject: cfg.normalized_public_url(),
        transcode,
        maintenance: maint,
        heartbeat: heartbeat.clone(),
        reset_token_ttl_ms: cfg.password_reset_ttl_hours * 3_600_000,
    };
    let worker = tokio::spawn(jobs::worker::run(
        birchtree::jobs::worker_ctx::WorkerCtx {
            store: store.clone(),
            clock: clock.clone(),
            mailer,
            blobs,
            streaming,
            federation,
            extra: worker_extra,
        },
        cfg.worker_poll_interval,
        jobs::worker::DEFAULT_BATCH,
        shutdown_rx,
    ));

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .unwrap_or_else(|err| {
        eprintln!("server error: {err}");
        std::process::exit(1);
    });

    // Signal the worker and wait for it to drain.
    let _ = shutdown_tx.send(true);
    let _ = worker.await;
    tracing::info!("birchtree stopped");
}

/// Base configuration: the `--config=PATH` JSON file over the compiled
/// defaults, when the flag is present (first occurrence wins). CLI flags
/// applied later by `parse_args` override file values. Load errors are fatal
/// and name the path.
fn config_base() -> Config {
    let path = std::env::args()
        .skip(1)
        .filter_map(|arg| arg.strip_prefix("--config=").map(str::to_string))
        .next();
    match path {
        None => Config::default(),
        Some(path) => Config::load_from_file(std::path::Path::new(&path)).unwrap_or_else(|err| {
            eprintln!("configuration error: {err}");
            std::process::exit(1);
        }),
    }
}

#[tokio::main]
async fn main() {
    observability::init();

    let mut cfg = config_base();
    let args = parse_args(&mut cfg);

    if let Err(err) = cfg.resolve_env_indirect() {
        eprintln!("configuration error: {err}");
        std::process::exit(1);
    }
    if let Err(err) = cfg.validate() {
        eprintln!("configuration error: {err}");
        std::process::exit(1);
    }
    if let Err(err) = cfg.check_runtime_mode() {
        eprintln!("configuration error: {err}");
        std::process::exit(2);
    }
    tracing::info!(summary = ?cfg.startup_summary(), "startup configuration");

    match args.positional.first().map(String::as_str) {
        Some("create-account") => run_create_account(&cfg, &args),
        Some("grant-role") | Some("set-role") => run_grant_role(&cfg, &args),
        Some("add-emoji") => run_add_emoji(&cfg, &args),
        Some("remove-emoji") | Some("rm-emoji") => run_remove_emoji(&cfg, &args),
        Some("list-emojis") => run_list_emojis(&cfg, &args),
        Some("help") => {
            print_usage();
            std::process::exit(0);
        }
        Some("create-rule") => run_create_rule(&cfg, &args),
        Some("create-text") => run_create_text(&cfg, &args),
        Some("set-rule-translation") => run_set_rule_translation(&cfg, &args),
        Some("delete-rule-translation") => run_delete_rule_translation(&cfg, &args),
        Some("delete-rule") => run_delete_rule(&cfg, &args),
        Some("list-rules") => run_list_rules(&cfg, &args),
        Some("crash-harness") => run_crash_harness(&cfg, &args),
        Some("sweep-transcode-orphans") => run_sweep_transcode_orphans(&cfg, &args),
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
        None => serve(cfg).await,
    }
}

/// `add-emoji --shortcode=<name> --file=<path> [--category=<cat>] [--hidden]`:
/// register (or replace) a local custom emoji from an image file (PNG/GIF/WebP,
/// at most 256 KB). `--hidden` keeps it out of the picker (still served by
/// URL). Missing flags exit 2; an unreadable file, bad shortcode, or
/// oversize/invalid image exits 1.
fn run_add_emoji(cfg: &Config, args: &Args) -> ! {
    let shortcode = args.shortcode.clone().unwrap_or_else(|| {
        eprintln!("add-emoji requires --shortcode=<name>");
        std::process::exit(2);
    });
    let path = args.file.clone().unwrap_or_else(|| {
        eprintln!("add-emoji requires --file=<path>");
        std::process::exit(2);
    });
    let image = std::fs::read(&path).unwrap_or_else(|err| {
        eprintln!("add-emoji: read {path}: {err}");
        std::process::exit(1);
    });
    let (store, _clock, _ids) = build_store(cfg);
    let blobs = birchtree::media::blob_store::FsBlobStore::open(cfg.data_dir.join("media"))
        .unwrap_or_else(|err| {
            eprintln!("open media store: {err}");
            std::process::exit(1);
        });
    match birchtree::application::emoji::add_emoji(
        &*store,
        &blobs,
        &shortcode,
        &image,
        args.category.as_deref(),
        !args.hidden,
    ) {
        Ok(emoji) => {
            println!("added emoji :{}:", emoji.shortcode);
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("add-emoji failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `remove-emoji --shortcode=<name>` (alias `rm-emoji`): delete a local custom
/// emoji and its image bytes. A missing shortcode is a not-found error
/// (exit 1), not a usage error: the invocation is well-formed but the row
/// does not exist (same convention as `delete-rule`).
fn run_remove_emoji(cfg: &Config, args: &Args) -> ! {
    let shortcode = args.shortcode.clone().unwrap_or_else(|| {
        eprintln!("remove-emoji requires --shortcode=<name>");
        std::process::exit(2);
    });
    let (store, _clock, _ids) = build_store(cfg);
    let blobs = birchtree::media::blob_store::FsBlobStore::open(cfg.data_dir.join("media"))
        .unwrap_or_else(|err| {
            eprintln!("open media store: {err}");
            std::process::exit(1);
        });
    match birchtree::application::emoji::rm_emoji(&*store, &blobs, &shortcode) {
        Ok(true) => {
            println!("removed emoji :{shortcode}:");
            std::process::exit(0);
        }
        Ok(false) => {
            eprintln!("remove-emoji: no emoji :{shortcode}:");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("remove-emoji failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `list-emojis`: print one line per emoji, shortcode-first:
/// `<shortcode> category=<cat|-> picker|hidden [disabled]`.
/// Currently lists picker-visible, enabled emoji only (`listed_emojis` is the
/// only enumeration on the `Store` port); hidden/disabled rows need the
/// proposed `all_emojis` port method before they can appear here.
fn run_list_emojis(cfg: &Config, _args: &Args) -> ! {
    use birchtree::application::ports::EmojiStore;
    let (store, _clock, _ids) = build_store(cfg);
    match store.all_emojis(1_000) {
        Ok(emojis) => {
            for emoji in emojis {
                println!(
                    "{} category={} {}{}",
                    emoji.shortcode,
                    emoji.category.as_deref().unwrap_or("-"),
                    if emoji.visible_in_picker {
                        "picker"
                    } else {
                        "hidden"
                    },
                    if emoji.disabled { " disabled" } else { "" },
                );
            }
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("list-emojis failed: {err}");
            std::process::exit(1);
        }
    }
}

/// Operator CLI usage (also printed for `help` and unknown subcommands).
fn print_usage() {
    println!(
        "\
usage: birchtree [global flags] <subcommand> [args]
subcommands:
  create-account --username=<name> --password=<pw> [--email=..] [--display-name=..] [--role=admin|moderator]
  grant-role --username=<name> --role=<admin|moderator|none>  (alias: set-role)
  add-emoji --shortcode=<name> --file=<path> [--category=<cat>] [--hidden]
  remove-emoji --shortcode=<name>  (alias: rm-emoji)
  list-emojis
  create-rule --text=<text> [--hint=<hint>]
  delete-rule --id=<id>
  list-rules
  crash-harness --mode=open-and-spin|commit-and-spin [--username=<name>]
  sweep-transcode-orphans [--media-id=<id> ...]
  help"
    );
}

/// `create-rule --text=<text> [--hint=<hint>]`: create one server rule
/// (D37; operator-managed, like custom emoji). Moderators attach rules to
/// reports through the admin report update. Prints the numeric id.
fn run_create_rule(cfg: &Config, args: &Args) -> ! {
    use birchtree::application::ports::AdminInfraStore;
    let text = args.text.clone().unwrap_or_else(|| {
        eprintln!("create-rule requires --text=<rule text>");
        std::process::exit(2);
    });
    let (store, clock, _ids) = build_store(cfg);
    match store.admin_create_rule(&text, args.hint.as_deref().unwrap_or(""), clock.now_ms()) {
        Ok(rule) => {
            println!("created rule id={}", rule.id);
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("create-rule failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `delete-rule --id=<id>`: delete one server rule (D37). Reports that
/// referenced it keep the stale id; serialization skips ids with no rule.
fn run_delete_rule(cfg: &Config, args: &Args) -> ! {
    use birchtree::application::ports::AdminInfraStore;
    use birchtree::domain::Id;
    let id: Id = args
        .id
        .as_deref()
        .unwrap_or("")
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("delete-rule requires --id=<rule id>");
            std::process::exit(2);
        });
    let (store, _clock, _ids) = build_store(cfg);
    match store.admin_delete_rule(id) {
        Ok(true) => {
            println!("deleted rule id={id}");
            std::process::exit(0);
        }
        Ok(false) => {
            eprintln!("delete-rule: no rule {id}");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("delete-rule failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `create-text --name=terms|privacy|extended [--file=<path>] --text=<text>`:
/// author one site-text file (G-A; companion to `--site-terms-file` etc.).
/// `--file` overrides the configured path for that name; otherwise the path
/// from config is written. Prints bytes written.
fn run_create_text(cfg: &Config, args: &Args) -> ! {
    let name = args.name.as_deref().unwrap_or_else(|| {
        eprintln!("create-text requires --name=terms|privacy|extended");
        std::process::exit(2);
    });
    let configured: Option<&std::path::PathBuf> = match name {
        "terms" => cfg.site_terms_file.as_ref(),
        "privacy" => cfg.site_privacy_file.as_ref(),
        "extended" => cfg.site_extended_file.as_ref(),
        _ => {
            eprintln!("create-text: unknown --name={name} (want terms|privacy|extended)");
            std::process::exit(2);
        }
    };
    let dest = args
        .file
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| configured.cloned())
        .unwrap_or_else(|| {
            eprintln!("create-text: no destination (set --file= or configure --site-{name}-file=)");
            std::process::exit(2);
        });
    let text = args.text.clone().unwrap_or_else(|| {
        eprintln!("create-text requires --text=<site text>");
        std::process::exit(2);
    });
    match std::fs::write(&dest, &text) {
        Ok(()) => {
            println!("wrote {} {} bytes to {}", name, text.len(), dest.display());
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("create-text failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `set-rule-translation --id=<rule> --locale=<tag> --text=<text> [--hint=<hint>]`: create or replace one rule translation (G-A).
fn run_set_rule_translation(cfg: &Config, args: &Args) -> ! {
    use birchtree::application::ports::AdminInfraStore;
    use birchtree::domain::Id;
    let id: Id = args
        .id
        .as_deref()
        .unwrap_or("")
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("set-rule-translation requires --id=<rule id>");
            std::process::exit(2);
        });
    let locale = args.locale.clone().unwrap_or_else(|| {
        eprintln!("set-rule-translation requires --locale=<BCP-47 tag>");
        std::process::exit(2);
    });
    let text = args.text.clone().unwrap_or_else(|| {
        eprintln!("set-rule-translation requires --text=<translation>");
        std::process::exit(2);
    });
    let (store, clock, _ids) = build_store(cfg);
    match store.admin_set_rule_translation(
        id,
        locale,
        text,
        args.hint.clone().unwrap_or_default(),
        clock.now_ms(),
    ) {
        Ok(row) => {
            println!("translated rule id={} locale={}", row.rule_id, row.locale);
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("set-rule-translation failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `delete-rule-translation --id=<rule> --locale=<tag>`: delete one rule translation (G-A).
fn run_delete_rule_translation(cfg: &Config, args: &Args) -> ! {
    use birchtree::application::ports::AdminInfraStore;
    use birchtree::domain::Id;
    let id: Id = args
        .id
        .as_deref()
        .unwrap_or("")
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("delete-rule-translation requires --id=<rule id>");
            std::process::exit(2);
        });
    let locale = args.locale.clone().unwrap_or_else(|| {
        eprintln!("delete-rule-translation requires --locale=<BCP-47 tag>");
        std::process::exit(2);
    });
    let (store, _clock, _ids) = build_store(cfg);
    match store.admin_delete_rule_translation(id, &locale) {
        Ok(true) => {
            println!("deleted translation rule id={id} locale={locale}");
            std::process::exit(0);
        }
        Ok(false) => {
            eprintln!("delete-rule-translation: no translation for rule {id} locale {locale}");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("delete-rule-translation failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `list-rules`: print every server rule id-first (D37 operator read).
fn run_list_rules(cfg: &Config, _args: &Args) -> ! {
    use birchtree::application::ports::AdminInfraStore;
    let (store, _clock, _ids) = build_store(cfg);
    match store.admin_rules() {
        Ok(rules) => {
            for rule in rules {
                println!("{} {}", rule.id, rule.text);
            }
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("list-rules failed: {err}");
            std::process::exit(1);
        }
    }
}

/// `sweep-transcode-orphans [--media-id=<id> ...]`: operator re-drive of the
/// orphan sweeper (Phase 7.1). Always sweeps stale `/tmp` transcode files;
/// each `--media-id=` additionally sweeps that attachment's staging blobs
/// when its row is terminal (missing/`Ready`/`Failed`). Prints counts.
fn run_sweep_transcode_orphans(cfg: &Config, args: &Args) -> ! {
    use birchtree::domain::Id;

    let tmp_removed = birchtree::media::transcode::sweep_tmp_files(
        &std::env::temp_dir(),
        birchtree::media::transcode::TMP_SWEEP_GRACE_MS,
        birchtree::media::transcode::TMP_SWEEP_CAP,
    );
    let (store, _clock, _ids) = build_store(cfg);
    let blobs = birchtree::media::blob_store::FsBlobStore::open(cfg.durable_dir_resolved())
        .unwrap_or_else(|err| {
            eprintln!("open media store: {err}");
            std::process::exit(1);
        });
    let mut staged_removed = 0usize;
    for raw in &args.positional[1..] {
        let Some(id_raw) = raw.strip_prefix("--media-id=") else {
            eprintln!("unknown sweep-transcode-orphans argument: {raw}");
            std::process::exit(2);
        };
        let id: Id = id_raw.parse().unwrap_or_else(|_| {
            eprintln!("invalid --media-id (want a numeric id): {id_raw}");
            std::process::exit(2);
        });
        match birchtree::media::transcode::sweep_staging_for_media(&*store, &blobs, id) {
            Ok(true) => staged_removed += 1,
            Ok(false) => {}
            Err(err) => {
                eprintln!("sweep media {id}: {err}");
                std::process::exit(1);
            }
        }
    }
    println!("swept {tmp_removed} temp files, {staged_removed} staged-blob orphans");
    std::process::exit(0);
}
