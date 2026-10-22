//! Fix-round verification: registration without SMTP never reports success,
//! and tracing spans never carry confirmation/reset tokens.
//!
//! - [`birchtree::mail::LoggingMailSender`] refuses every send (retryable
//!   error for the worker, never `Ok`), and logs the recipient domain only.
//! - [`birchtree::application::accounts::check_registration_mail`] fails
//!   signup closed when no provider is configured.
//! - [`birchtree::mailer::require_mail_for_open_registration`] refuses
//!   production startup with open registrations and no SMTP.
//! - [`birchtree::mailer::seal_failure_is_fatal`] is fatal in production.
//! - [`birchtree::observability::redacted_path`] drops the query string that
//!   carries `confirmation_token` / `reset_password_token`.

use std::sync::Arc;

use birchtree::application::accounts::check_registration_mail;
use birchtree::application::ports::MailSender;
use birchtree::config::Config;
use birchtree::config::RuntimeMode;
use birchtree::domain::Id;
use birchtree::mail::LoggingMailSender;
use birchtree::mail::SmtpJobSender;
use birchtree::mailer::DisabledMailer;
use birchtree::mailer::MailEnv;
use birchtree::mailer::MailMessage;
use birchtree::mailer::mailer_from_config;
use birchtree::mailer::open_registration_without_mail;
use birchtree::mailer::require_mail_for_open_registration;
use birchtree::mailer::seal_failure_is_fatal;
use birchtree::observability::redacted_path;

#[test]
fn logging_adapter_never_reports_success() {
    let sender = LoggingMailSender;
    let err = sender
        .send_confirmation("user@example.com", Id::new(1), "one-time-token")
        .expect_err("no SMTP provider must refuse, never fake success");
    assert!(err.to_string().contains("not configured"), "{err}");
    let err = sender
        .send_password_reset("user@example.com", Id::new(1), "one-time-token")
        .expect_err("no SMTP provider must refuse, never fake success");
    assert!(err.to_string().contains("not configured"), "{err}");
}

#[test]
fn smtp_job_sender_with_disabled_backend_refuses() {
    let backend: Arc<dyn birchtree::mailer::Mailer> = Arc::new(DisabledMailer);
    let sender = SmtpJobSender::new(backend, "noreply@example.social", "https://example.social");
    assert!(
        sender
            .send_confirmation("user@example.com", Id::new(1), "token")
            .is_err()
    );
}

#[test]
fn unconfigured_mailer_is_disabled_not_fake_success() {
    let mailer = mailer_from_config(None, None).expect("builds");
    let err = mailer
        .send(&MailMessage {
            from: "a@example.social".to_string(),
            to: "user@example.com".to_string(),
            subject: "s".to_string(),
            body_text: "b".to_string(),
        })
        .expect_err("disabled backend must fail");
    assert!(matches!(err, birchtree::mailer::MailError::Disabled(_)));
}

#[test]
fn registration_guard_fails_closed_without_mail() {
    assert!(check_registration_mail(&MailEnv::new("noreply@example.social", true)).is_ok());
    assert!(check_registration_mail(&MailEnv::disabled()).is_err());
}

#[test]
fn open_registration_without_smtp_refuses_production_startup() {
    let open_no_smtp = Config {
        registration_open: true,
        ..Config::default()
    };
    assert!(open_registration_without_mail(&open_no_smtp));
    assert!(require_mail_for_open_registration(&open_no_smtp).is_err());

    let lab = Config {
        mode: RuntimeMode::Lab,
        ..open_no_smtp.clone()
    };
    assert!(require_mail_for_open_registration(&lab).is_ok());

    let closed = Config::default();
    assert!(!open_registration_without_mail(&closed));
    assert!(require_mail_for_open_registration(&closed).is_ok());

    let with_smtp = Config {
        registration_open: true,
        smtp_url: Some("smtp://127.0.0.1:25".to_string()),
        mail_from: Some("noreply@example.social".to_string()),
        ..Config::default()
    };
    assert!(!open_registration_without_mail(&with_smtp));
    assert!(require_mail_for_open_registration(&with_smtp).is_ok());
}

#[test]
fn seal_failure_is_fatal_in_production_only() {
    assert!(seal_failure_is_fatal(RuntimeMode::Production));
    assert!(!seal_failure_is_fatal(RuntimeMode::Lab));
}

#[test]
fn span_path_never_carries_token_query() {
    for (raw, expected) in [
        (
            "http://x/auth/confirmation?confirmation_token=abc123",
            "/auth/confirmation",
        ),
        (
            "http://x/auth/password/edit?reset_password_token=abc123",
            "/auth/password/edit",
        ),
        ("http://x/api/v1/accounts?limit=20", "/api/v1/accounts"),
        ("http://x/health", "/health"),
    ] {
        let uri: axum::http::Uri = raw.parse().expect("test uri");
        assert_eq!(redacted_path(&uri), expected, "{raw}");
        assert!(!redacted_path(&uri).contains("abc123"), "{raw}");
    }
}
