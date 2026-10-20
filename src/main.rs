use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use birchtree::api::mastodon::{build_router, ApiState, ServerInfo};
use birchtree::application::accounts::{create_account, CreateAccountCommand};
use birchtree::application::context::Context;
use birchtree::application::ports::MailSender;
use birchtree::config::Config;
use birchtree::domain::id::{SystemClock, TimestampIdGenerator};
use birchtree::domain::{Clock, IdGenerator};
use birchtree::jobs;
use birchtree::mail::LoggingMailSender;
use birchtree::observability;
use birchtree::persistence::redb::RedbStore;

/// Parsed command-line arguments.
#[derive(Default)]
struct Args {
    positional: Vec<String>,
    username: Option<String>,
    password: Option<String>,
    email: Option<String>,
    display_name: Option<String>,
    role: Option<String>,
    harness_mode: Option<String>,
    text: Option<String>,
    hint: Option<String>,
    id: Option<String>,
}

fn parse_args(cfg: &mut Config) -> Args {
    let mut args = Args::default();
    for arg in std::env::args().skip(1) {
        if let Some(_path) = arg.strip_prefix("--config=") {
            // Consumed before parsing (sets the base config this call
            // overrides); see main().
        } else if let Some(v) = arg.strip_prefix("--public-url=") {
            cfg.public_url = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--data-dir=") {
            cfg.data_dir = v.into();
        } else if let Some(v) = arg.strip_prefix("--listen=") {
            cfg.listen = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --listen address: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--trusted-proxy=") {
            cfg.trusted_proxies.push(v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --trusted-proxy address: {v}");
                std::process::exit(2);
            }));
        } else if let Some(v) = arg.strip_prefix("--max-body-size=") {
            cfg.max_body_size = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --max-body-size: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--max-status-chars=") {
            cfg.max_status_chars = v.parse().ok().filter(|m| *m >= 1).unwrap_or_else(|| {
                eprintln!("invalid --max-status-chars (want an integer 1..=3000): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--username=") {
            args.username = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--password=") {
            args.password = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--email=") {
            args.email = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--display-name=") {
            args.display_name = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--role=") {
            args.role = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--text=") {
            args.text = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--hint=") {
            args.hint = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--id=") {
            args.id = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--site-title=") {
            cfg.site_title = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-description=") {
            cfg.site_description = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-contact-email=") {
            cfg.site_contact_email = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--streaming-url=") {
            cfg.streaming_api_base_url = Some(v.trim_end_matches('/').to_string());
        } else if let Some(v) = arg.strip_prefix("--site-contact-username=") {
            cfg.site_contact_username = Some(v.trim_start_matches('@').to_string());
        } else if arg == "--federation-allow-http" {
            cfg.federation_allow_http = true;
        } else if arg == "--federation-authorized-fetch" {
            cfg.federation_authorized_fetch = true;
        } else if let Some(v) = arg.strip_prefix("--federation-extra-ca=") {
            cfg.federation_extra_ca = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--federation-allow-private=") {
            cfg.federation_allowlist = birchtree::federation::policy::parse_allowlist(v).unwrap_or_else(|err| {
                eprintln!("invalid --federation-allow-private: {err}");
                std::process::exit(2);
            });
        } else if arg == "--debug-weak-rate-limits" {
            cfg.rate_limit_multiplier = 10;
        } else if let Some(v) = arg.strip_prefix("--rate-limit-multiplier=") {
            cfg.rate_limit_multiplier = v.parse().ok().filter(|m| *m >= 1).unwrap_or_else(|| {
                eprintln!("invalid --rate-limit-multiplier (want an integer >= 1): {v}");
                std::process::exit(2);
            });
        } else if arg == "--registration-open" {
            cfg.registration_open = true;
        } else if arg == "--registration-closed" {
            cfg.registration_open = false;
        } else if arg == "--registration-require-approval" {
            cfg.registration_requires_approval = true;
        } else if let Some(v) = arg.strip_prefix("--search-mode=") {
            cfg.search_mode =
                birchtree::search::SearchMode::parse(v).unwrap_or_else(|| {
                    eprintln!("invalid --search-mode (want embedded|database|off): {v}");
                    std::process::exit(2);
                });
        } else if arg == "--no-transcode" {
            cfg.transcoding_enabled = false;
        } else if let Some(v) = arg.strip_prefix("--ffmpeg-path=") {
            cfg.ffmpeg_path = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--webp-quality=") {
            cfg.webp_quality = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --webp-quality (want 0-100): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vp9-crf=") {
            cfg.vp9_crf = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --vp9-crf (want 0-63): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vp9-cpu-used=") {
            cfg.vp9_cpu_used = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --vp9-cpu-used (want 0-5): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--opus-bitrate-kbps=") {
            cfg.opus_bitrate_kbps = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --opus-bitrate-kbps (want an integer > 0): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-wall-time-secs=") {
            cfg.transcode_wall_time_secs = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-wall-time-secs: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-max-output-bytes=") {
            cfg.transcode_max_output_bytes = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-max-output-bytes: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-max-memory-bytes=") {
            cfg.transcode_max_memory_bytes = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-max-memory-bytes: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vapid-public-key=") {
            cfg.vapid_public_key = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--vapid-private-key=") {
            cfg.vapid_private_key = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--smtp-url=") {
            cfg.smtp_url = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--mail-from=") {
            cfg.mail_from = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--web-client-dir=") {
            cfg.web_client_dir = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--mode=") {
            args.harness_mode = Some(v.to_string());
        } else if arg.starts_with("--") {
            eprintln!("unknown flag: {arg}");
            std::process::exit(2);
        } else {
            args.positional.push(arg);
        }
    }
    args
}

fn build_store(cfg: &Config) -> (Arc<RedbStore>, Arc<dyn Clock>, Arc<dyn IdGenerator>) {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let store = RedbStore::open(cfg.database_path(), clock.clone(), ids.clone())
        .unwrap_or_else(|err| {
            eprintln!("open storage: {err}");
            std::process::exit(1);
        });
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
            println!("created account id={} username={}", account.id, account.username);
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
            println!("granted role={label} account id={} username={}", account.id, account.username);
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
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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
    let blobs: Arc<dyn birchtree::application::ports::BlobStore> =
        Arc::new(birchtree::media::blob_store::FsBlobStore::open(cfg.data_dir.join("media"))
            .unwrap_or_else(|err| {
                eprintln!("open blob store: {err}");
                std::process::exit(1);
            }));
    let server = Arc::new(ServerInfo::from_config(&cfg));
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
    }    let streams = birchtree::streaming::Hub::new(
        store.clone(),
        clock.clone(),
        server.clone(),
        birchtree::streaming::Limits::default(),
    );
    let streaming: Arc<dyn birchtree::application::ports::StreamingSink> =
        Arc::new(streams.clone());
    let context = Arc::new(Context::new(
        clock.clone(),
        ids,
        store.clone(),
        blobs.clone(),
        streaming.clone(),
    ));
    // Federation (Phase 6). The allowlist and http switches are lab-only and off by default.
    let lab = !cfg.federation_allowlist.is_empty();
    if cfg.federation_allow_http || lab {
        tracing::warn!(
            allow_http = cfg.federation_allow_http,
            allowlist_entries = cfg.federation_allowlist.len(),
            "lab federation policy is in force: not for production"
        );
    }
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
    let federation = birchtree::activitypub::build_config(
        context.clone(),
        &cfg.normalized_public_url(),
        cfg.registration_open,
        &cfg.site_title,
        &cfg.site_description,
        &http_policy,
        lab,
        Some(&instance),
        cfg.federation_authorized_fetch,
    )
    .await
    .unwrap_or_else(|err| {
        eprintln!("start federation: {err}");
        std::process::exit(1);
    });
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(cfg.trusted_proxies.clone()),
        max_body_size: cfg.max_body_size,
        server,
        streams,
        federation: federation.clone(),
        rate_limit_multiplier: cfg.rate_limit_multiplier,
        mail_env: Arc::new(birchtree::mailer::MailEnv::from_config(&cfg)),
        invites: store.clone() as std::sync::Arc<dyn birchtree::application::accounts::InviteStore>,
    };
    if cfg.rate_limit_multiplier > 1 {
        tracing::warn!(multiplier = cfg.rate_limit_multiplier, "debug rate-limit weakening is in force: not for production");
    }
    let app = build_router(state);
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

    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .unwrap_or_else(|err| {
            eprintln!("bind {}: {err}", cfg.listen);
            std::process::exit(1);
        });

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
        push_client: reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|err| {
                eprintln!("build push http client: {err}");
                std::process::exit(1);
            }),
        vapid: birchtree::application::push::VapidKeys {
            public_b64: cfg.vapid_public_key.clone(),
            private_b64: cfg.vapid_private_key.clone(),
        },
        push_subject: cfg.normalized_public_url(),
        transcode,
    };
    let worker = tokio::spawn(jobs::worker::run(
        store.clone(),
        clock.clone(),
        mailer,
        blobs,
        streaming,
        federation,
        worker_extra,
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

    match args.positional.first().map(String::as_str) {
        Some("create-account") => run_create_account(&cfg, &args),
        Some("grant-role") | Some("set-role") => run_grant_role(&cfg, &args),
        Some("create-rule") => run_create_rule(&cfg, &args),
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
    let id: Id = args.id.as_deref().unwrap_or("").parse().unwrap_or_else(|_| {
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
    let blobs = birchtree::media::blob_store::FsBlobStore::open(cfg.data_dir.join("media"))
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
