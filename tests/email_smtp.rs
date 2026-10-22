//! End-to-end outbound mail: real SMTP delivery against a fake sink,
//! durable-job delivery through the worker handler, and endpoint enqueue
//! behavior when enabled. Nothing here touches the network.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::auth;
use birchtree::application::context::Context;
use birchtree::application::ports::{AccountStore, JobQueue};
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use common::{
    T0, TEST_PUBLIC_URL, TestServer, federation_config, form_request, get_request, send,
    server_info,
};

/// A minimal SMTP sink: greets, accepts one message, records the DATA.
/// Speaks just enough for lettre's sync client (EHLO, MAIL, RCPT, DATA,
/// QUIT). No TLS — tests use loopback plaintext, which production refuses
/// for non-loopback hosts.
struct Sink {
    addr: std::net::SocketAddr,
    data: Arc<Mutex<Vec<String>>>,
}

async fn start_sink() -> Sink {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let data: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let data_clone = data.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            handle_conn(stream, data_clone.clone()).await;
        }
    });
    Sink { addr, data }
}

async fn handle_conn(stream: TcpStream, data: Arc<Mutex<Vec<String>>>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    writer.write_all(b"220 fake.test ESMTP\r\n").await.ok();
    let mut in_data = false;
    let mut body = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if in_data {
            if line == "." {
                in_data = false;
                data.lock().expect("sink").push(std::mem::take(&mut body));
                writer.write_all(b"250 ok\r\n").await.ok();
            } else {
                body.push_str(&line);
                body.push('\n');
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            writer
                .write_all(b"250-Hello\r\n250 8BITMIME\r\n")
                .await
                .ok();
        } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
            writer.write_all(b"250 ok\r\n").await.ok();
        } else if upper == "DATA" {
            in_data = true;
            writer.write_all(b"354 end with .\r\n").await.ok();
        } else if upper == "QUIT" || upper == "RSET" {
            let reply = if upper == "QUIT" {
                b"221 bye\r\n".as_slice()
            } else {
                b"250 ok\r\n".as_slice()
            };
            writer.write_all(reply).await.ok();
            if upper == "QUIT" {
                break;
            }
        } else {
            writer.write_all(b"502 unimplemented\r\n").await.ok();
        }
    }
}

fn smtp_mailer(addr: std::net::SocketAddr) -> birchtree::mailer::SmtpMailer {
    birchtree::mailer::SmtpMailer::new(&format!("smtp://{addr}"), "noreply@example.social")
        .expect("test mailer builds")
}

fn user_token(srv: &TestServer, username: &str, email: Option<&str>) -> String {
    use birchtree::application::accounts::{CreateAccountCommand, create_account};
    let account = match srv
        .store
        .account_by_handle(username, "example.social")
        .expect("store")
    {
        Some(account) => account,
        None => create_account(
            &*srv.store,
            "https://example.social",
            T0,
            CreateAccountCommand {
                username: username.to_string(),
                email: email.map(str::to_string),
                password: "password123".to_string(),
                display_name: None,
            },
        )
        .expect("create account"),
    };
    let app = auth::register_app(
        &*srv.store,
        format!("Email {username}"),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("register app");
    let (plaintext, _) = auth::issue_user_token(
        &*srv.store,
        app.app.id,
        account.id,
        ["read".to_string(), "write".to_string()].to_vec(),
        T0,
    )
    .expect("token");
    plaintext
}

/// Rebuild the full router sharing the server's store, with mail enabled.
fn enabled_router(srv: &TestServer, from: &str) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let streams = srv.streams.clone();
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(streams.clone()),
    ));
    let server: ServerInfo = (*server_info()).clone();
    let federation = federation_config(context.clone(), TEST_PUBLIC_URL);
    birchtree::api::mastodon::build_router(ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server: Arc::new(server),
        streams: streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: Arc::new(birchtree::mailer::MailEnv::new(from, true)),
        invites: std::sync::Arc::new(
            birchtree::application::accounts::MemoryInviteStore::default(),
        ),
    })
}

fn last_data(sink: &Sink) -> String {
    sink.data
        .lock()
        .expect("sink")
        .last()
        .cloned()
        .expect("one message")
}

/// Undo quoted-printable soft line breaks (`=\n`) and the escaped `=`
/// (`=3D`) so assertions can match long token lines regardless of wrapping.
fn unfold_qp(body: &str) -> String {
    body.replace("=\n", "").replace("=3D", "=")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_send_reaches_fake_sink() {
    use birchtree::mailer::{MailMessage, Mailer};
    let sink = start_sink().await;
    let mailer = smtp_mailer(sink.addr);
    mailer
        .send(&MailMessage {
            from: "noreply@example.social".into(),
            to: "user@example.com".into(),
            subject: "Reset password instructions".into(),
            body_text: "token abc123".into(),
        })
        .expect("delivers to the fake sink");
    let got = last_data(&sink);
    assert!(got.contains("Reset password instructions"), "{got}");
    assert!(got.contains("abc123"), "{got}");
    assert!(got.contains("noreply@example.social"), "{got}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_job_delivers_through_worker_handler() {
    let sink = start_sink().await;
    let srv = TestServer::new(true);
    let mailer = smtp_mailer(sink.addr);
    let bridge = birchtree::mail::SmtpJobSender::new(
        Arc::new(mailer),
        "noreply@example.social",
        "https://example.social",
    );
    let job = Job::new(
        Id::new(999),
        JobKind::SendPasswordResetEmail,
        serde_json::json!({
            "account_id": 123u64,
            "email": "user@example.com",
            "token": "reset-token-xyz",
        }),
        0,
        0,
    );
    birchtree::jobs::handlers::handle(
        &job,
        &*srv.store,
        &bridge,
        &*srv.blobs,
        &birchtree::application::ports::NoStreaming,
    )
    .expect("handler delivers");
    let got = unfold_qp(&last_data(&sink));
    assert!(
        got.contains("reset_password_token=reset-token-xyz"),
        "{got}"
    );
    assert!(
        got.contains("https://example.social/auth/password/edit"),
        "{got}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmation_endpoint_enqueues_and_worker_delivers() {
    let sink = start_sink().await;
    let srv = TestServer::new(true);
    let router = enabled_router(&srv, "noreply@example.social");
    let user = user_token(&srv, "alice", Some("alice@example.com"));

    // The request only enqueues (no network in the handler).
    let (status, _, bytes) = send(
        &router,
        form_request("POST", "/api/v1/emails/confirmations", "", Some(&user)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    // The job carries a random (unguessable) token.
    let jobs = srv.store.claim_due(4_000_000_000_000, 64).expect("claim");
    let job = jobs
        .iter()
        .find(|j| j.kind == JobKind::SendConfirmationEmail)
        .expect("confirmation job queued");
    let token = job.payload["token"].as_str().expect("token in payload");
    assert_eq!(token.len(), 64, "32 random bytes as hex");

    // The worker delivers it to the fake sink.
    let mailer = smtp_mailer(sink.addr);
    let bridge = birchtree::mail::SmtpJobSender::new(
        Arc::new(mailer),
        "noreply@example.social",
        "https://example.social",
    );
    birchtree::jobs::handlers::handle(
        job,
        &*srv.store,
        &bridge,
        &*srv.blobs,
        &birchtree::application::ports::NoStreaming,
    )
    .expect("handler delivers");
    srv.store.complete(job.id).expect("complete");
    let got = unfold_qp(&last_data(&sink));
    assert!(
        got.contains(&format!("confirmation_token={token}")),
        "{got}"
    );
}

#[tokio::test]
async fn confirmation_endpoint_still_refuses_when_disabled() {
    // An explicitly mail-disabled router (the default test router enables
    // mail so registration tests exercise the fail-closed guard's pass path).
    let srv = TestServer::new(true);
    let router = srv.disabled_mail_router();
    let user = user_token(&srv, "bob", Some("bob@example.com"));
    let (status, _, _) = send(
        &router,
        form_request("POST", "/api/v1/emails/confirmations", "", Some(&user)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // Nothing was queued behind the refusal.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/emails/check_confirmation", Some(&user), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}
