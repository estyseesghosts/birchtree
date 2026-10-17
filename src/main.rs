use std::net::SocketAddr;
use std::sync::Arc;

use birchtree::api::mastodon::{build_router, ApiState};
use birchtree::application::ports::{Clock, IdGenerator};
use birchtree::config::Config;
use birchtree::domain::id::{SystemClock, TimestampIdGenerator};
use birchtree::observability;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::hub::{hub_and_sink, Hub};

fn build_services(
    cfg: &Config,
) -> Result<(Arc<birchtree::application::context::Context>, Arc<Hub>), String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let _store = RedbStore::open(cfg.data_dir.join("birchtree.redb"), clock.clone(), ids.clone())
        .map_err(|err| format!("open redb store: {err}"))?;
    let (_hub, _streaming) = hub_and_sink(1024);
    let context = Arc::new(birchtree::application::context::Context::new(clock, ids));
    Ok((context, Arc::new(Hub::new())))
}

/// `create-account --username=<name> --password=<pw>`: create a local account
/// from the CLI for operators (Phase 1 gap).
fn run_create_account(_args: &[String], _cfg: &Config) -> ! {
    eprintln!("create-account: not yet implemented (Phase 1)");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    observability::init();

    let mut cfg = Config::default();
    let mut positional: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(mode) = arg.strip_prefix("--mode=") {
            let _ = mode;
        } else if let Some(url) = arg.strip_prefix("--public-url=") {
            cfg.public_url = url.to_string();
        } else if let Some(dir) = arg.strip_prefix("--data-dir=") {
            cfg.data_dir = dir.into();
        } else if let Some(listen) = arg.strip_prefix("--listen=") {
            cfg.listen = listen.parse().expect("valid --listen addr");
        } else {
            positional.push(arg);
        }
    }

    if let Err(err) = cfg.validate() {
        eprintln!("{err}");
        std::process::exit(1);
    }

    if positional.first().is_some_and(|cmd| cmd == "create-account") {
        run_create_account(&positional[1..], &cfg);
    } else if let Some(cmd) = positional.first() {
        eprintln!("unknown subcommand: {cmd}");
        std::process::exit(2);
    }

    let (context, _hub) = build_services(&cfg).unwrap_or_else(|err| {
        eprintln!("{err}");
        std::process::exit(1);
    });
    let api_state = ApiState {
        context,
        trusted_proxies: Arc::new(cfg.trusted_proxies.clone()),
    };
    let app = build_router(api_state);
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .expect("bind listener");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("serve axum");
}
