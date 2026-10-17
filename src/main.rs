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
    harness_mode: Option<String>,
}

fn parse_args(cfg: &mut Config) -> Args {
    let mut args = Args::default();
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--public-url=") {
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
        } else if let Some(v) = arg.strip_prefix("--username=") {
            args.username = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--password=") {
            args.password = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--email=") {
            args.email = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--display-name=") {
            args.display_name = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--site-title=") {
            cfg.site_title = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-description=") {
            cfg.site_description = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-contact-email=") {
            cfg.site_contact_email = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-contact-username=") {
            cfg.site_contact_username = Some(v.trim_start_matches('@').to_string());
        } else if arg == "--registration-open" {
            cfg.registration_open = true;
        } else if arg == "--registration-closed" {
            cfg.registration_open = false;
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
/// from the CLI for operators (the Phase 1 bootstrap path).
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

    let (store, clock, _ids) = build_store(cfg);
    let cmd = CreateAccountCommand {
        username,
        email: args.email.clone(),
        password,
        display_name: args.display_name.clone(),
    };
    match create_account(&*store, &cfg.normalized_public_url(), clock.now_ms(), cmd) {
        Ok(account) => {
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
    let context = Arc::new(Context::new(clock.clone(), ids, store.clone()));
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(cfg.trusted_proxies.clone()),
        max_body_size: cfg.max_body_size,
        server,
    };
    let app = build_router(state);
    let mailer: Arc<dyn MailSender> = Arc::new(LoggingMailSender);

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
    let worker = tokio::spawn(jobs::worker::run(
        store.clone(),
        clock.clone(),
        mailer,
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

#[tokio::main]
async fn main() {
    observability::init();

    let mut cfg = Config::default();
    let args = parse_args(&mut cfg);

    if let Err(err) = cfg.validate() {
        eprintln!("configuration error: {err}");
        std::process::exit(1);
    }

    match args.positional.first().map(String::as_str) {
        Some("create-account") => run_create_account(&cfg, &args),
        Some("crash-harness") => run_crash_harness(&cfg, &args),
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
        None => serve(cfg).await,
    }
}
