//! Pluggable outbound mail backends (round 3, area A).
//!
//! Upstream shapes: `app/mailers/user_mailer.rb` at v4.7.3 (`8ff11c2b`) —
//! `confirmation_instructions(user, token)` (`user_mailer.rb:23-36`, subject
//! `devise.mailer.confirmation_instructions.subject`, sent only when the
//! account `active_for_authentication?`) and
//! `reset_password_instructions(user, token)` (`user_mailer.rb:38-46`).
//! Devise's default subjects are "Confirmation instructions" and
//! "Reset password instructions"; the links carry the one-time token as
//! `/auth/confirmation?confirmation_token=…` and
//! `/auth/password/edit?reset_password_token=…`.
//!
//! Architecture: mail is sent AFTER commit, never inside a request
//! transaction — the resend endpoints enqueue durable jobs and the worker
//! delivers. Without an SMTP provider the mailer is explicitly disabled:
//! sends fail with [`MailError::Disabled`] (surfaced as `503`), never a
//! silent fake success. [`LogMailer`] is the test/dev backend: it stores
//! messages in an observable outbox (no network) so tests can assert the
//! send path ran. [`SmtpMailer`] delivers for real through `lettre`.
//!
//! Privacy: tokens are one-time secrets and recipient addresses are PII —
//! neither is ever logged. Log lines carry only the message kind and the
//! recipient domain.
//!
//! [`SmtpMailer`] delivers for real through `lettre` (synchronous transport
//! run in [`tokio::task::block_in_place`], so it must run on a multi-thread
//! runtime — the production worker is one; sync drains in tests keep the log
//! backend). `smtp://` requires STARTTLS except on loopback (lab/testing);
//! `smtps://` uses implicit TLS.

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::config::RuntimeMode;
use crate::domain::Id;

/// One outbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailMessage {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body_text: String,
}

impl MailMessage {
    /// Recipient domain for log lines (never the full address).
    pub fn to_domain(&self) -> &str {
        self.to.rsplit('@').next().unwrap_or("")
    }
}

/// How a send failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailError {
    /// No mail provider is configured. Explicit disabled state — never a
    /// silent success.
    Disabled(&'static str),
    /// The recipient address is unusable.
    InvalidRecipient(String),
    /// The provider refused or the transport is not implemented.
    Transport(String),
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MailError::Disabled(msg) => write!(f, "email delivery is disabled: {msg}"),
            MailError::InvalidRecipient(msg) => write!(f, "invalid recipient: {msg}"),
            MailError::Transport(msg) => write!(f, "email delivery failed: {msg}"),
        }
    }
}

impl std::error::Error for MailError {}

impl From<MailError> for crate::domain::Error {
    fn from(err: MailError) -> Self {
        match err {
            MailError::Disabled(msg) => {
                crate::domain::Error::Forbidden(format!("Email delivery is not configured ({msg})"))
            }
            MailError::InvalidRecipient(msg) => {
                crate::domain::Error::InvalidInput(format!("email {msg}"))
            }
            MailError::Transport(msg) => crate::domain::Error::Internal(msg),
        }
    }
}

/// Outbound mail backend (object-safe so routers can hold `Arc<dyn Mailer>`).
pub trait Mailer: Send + Sync {
    /// Deliver one message. Implementations never log `to` or the body.
    fn send(&self, msg: &MailMessage) -> Result<(), MailError>;

    /// Confirmation mail for a newly registered account (upstream
    /// `UserMailer#confirmation_instructions`, `user_mailer.rb:23-36`).
    fn send_confirmation(
        &self,
        from: &str,
        to: &str,
        _account_id: Id,
        token: &str,
        base_url: &str,
    ) -> Result<(), MailError> {
        validate_recipient(to)?;
        let base = base_url.trim_end_matches('/');
        self.send(&MailMessage {
            from: from.to_string(),
            to: to.to_string(),
            subject: "Confirmation instructions".to_string(),
            body_text: format!(
                "Confirm your account by visiting:\n\n\
                 {base}/auth/confirmation?confirmation_token={token}\n\n\
                 If you did not create this account, ignore this message.\n"
            ),
        })
    }

    /// Password-reset mail (upstream
    /// `UserMailer#reset_password_instructions`, `user_mailer.rb:38-46`).
    fn send_password_reset(
        &self,
        from: &str,
        to: &str,
        _account_id: Id,
        token: &str,
        base_url: &str,
    ) -> Result<(), MailError> {
        validate_recipient(to)?;
        let base = base_url.trim_end_matches('/');
        self.send(&MailMessage {
            from: from.to_string(),
            to: to.to_string(),
            subject: "Reset password instructions".to_string(),
            body_text: format!(
                "Reset your password by visiting:\n\n\
                 {base}/auth/password/edit?reset_password_token={token}\n\n\
                 If you did not request this, ignore this message.\n"
            ),
        })
    }
}

/// Minimal recipient check shared by the provided send paths.
pub fn validate_address(to: &str) -> Result<(), MailError> {
    validate_recipient(to)
}

fn validate_recipient(to: &str) -> Result<(), MailError> {
    let to = to.trim();
    let ok = !to.is_empty()
        && !to.contains(char::is_whitespace)
        && to
            .split('@')
            .nth(1)
            .is_some_and(|d| !d.is_empty() && d.contains('.'));
    if ok {
        Ok(())
    } else {
        Err(MailError::InvalidRecipient(
            "address is invalid".to_string(),
        ))
    }
}

/// Test/dev backend: stores messages in an observable outbox, sends nothing.
///
/// Returning `Ok` here is honest, not a fake success: the outbox IS the
/// delivery record for tests and local development, and production never
/// wires this backend (unconfigured SMTP yields [`DisabledMailer`]).
#[derive(Debug, Default)]
pub struct LogMailer {
    outbox: Mutex<Vec<MailMessage>>,
}

impl LogMailer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of queued messages (tests assert the send path ran).
    pub fn outbox(&self) -> Vec<MailMessage> {
        self.outbox.lock().expect("mail outbox").clone()
    }

    /// Number of queued messages.
    pub fn len(&self) -> usize {
        self.outbox.lock().expect("mail outbox").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Mailer for LogMailer {
    fn send(&self, msg: &MailMessage) -> Result<(), MailError> {
        self.outbox.lock().expect("mail outbox").push(msg.clone());
        tracing::info!(
            subject = %msg.subject,
            recipient_domain = %msg.to_domain(),
            "mail queued (log backend; no SMTP delivery)"
        );
        Ok(())
    }
}

/// Explicit disabled state: every send fails with [`MailError::Disabled`].
///
/// This is what production wires when no SMTP provider is configured — the
/// HTTP layer turns it into `503 {"error":"Email delivery is not
/// configured"}`, never a silent success.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledMailer;

impl Mailer for DisabledMailer {
    fn send(&self, _msg: &MailMessage) -> Result<(), MailError> {
        Err(MailError::Disabled(
            "no SMTP provider configured (set --smtp-url and --mail-from)",
        ))
    }
}

/// Real SMTP backend. Construction parses (no network, no secrets kept
/// beyond the credentials needed to authenticate); `send` delivers.
///
/// URL shapes: `smtp://[user:pass@]host[:port]` (STARTTLS required unless the
/// host is loopback) and `smtps://[user:pass@]host[:port]` (implicit TLS).
/// Default ports 587 / 465. Anything else (query strings, other schemes) is
/// refused at construction.
#[derive(Debug, Clone)]
pub struct SmtpMailer {
    from: String,
    host: String,
    port: u16,
    implicit_tls: bool,
    allow_plaintext: bool,
    credentials: Option<(String, String)>,
}

impl SmtpMailer {
    /// Validate `smtp://…` / `smtps://…` URL shape (no network, no secrets).
    pub fn new(url: &str, from: &str) -> Result<Self, String> {
        if !from.contains('@') {
            return Err("--mail-from must be an email address".to_string());
        }
        let parsed = url::Url::parse(url)
            .map_err(|_| "--smtp-url must start with smtp:// or smtps://".to_string())?;
        let implicit_tls = match parsed.scheme() {
            "smtps" => true,
            "smtp" => false,
            _ => return Err("--smtp-url must start with smtp:// or smtps://".to_string()),
        };
        if parsed.query().is_some() {
            return Err("--smtp-url must not carry a query string".to_string());
        }
        let host = parsed
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| "--smtp-url needs a host".to_string())?
            .to_string();
        let port = parsed
            .port()
            .unwrap_or(if implicit_tls { 465 } else { 587 });
        let allow_plaintext = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
        let credentials = match (parsed.username(), parsed.password()) {
            ("", _) => None,
            (user, pass) => {
                let user = percent_decode(user);
                let pass = pass.map(percent_decode).unwrap_or_default();
                if user.is_empty() {
                    return Err("--smtp-url has an empty username".to_string());
                }
                Some((user, pass))
            }
        };
        Ok(Self {
            from: from.to_string(),
            host,
            port,
            implicit_tls,
            allow_plaintext,
            credentials,
        })
    }

    pub fn from_address(&self) -> &str {
        &self.from
    }

    fn transport(&self) -> Result<lettre::SmtpTransport, MailError> {
        use lettre::transport::smtp::client::{Tls, TlsParameters};
        let tls_params = TlsParameters::new(self.host.clone())
            .map_err(|err| MailError::Transport(format!("smtp tls setup failed: {err}")))?;
        let mut builder = if self.implicit_tls {
            lettre::SmtpTransport::builder_dangerous(&self.host)
                .port(self.port)
                .tls(Tls::Wrapper(tls_params))
        } else if self.allow_plaintext {
            lettre::SmtpTransport::builder_dangerous(&self.host).port(self.port)
        } else {
            lettre::SmtpTransport::relay(&self.host)
                .map_err(|err| MailError::Transport(format!("smtp setup failed: {err}")))?
                .port(self.port)
                .tls(Tls::Required(tls_params))
        };
        builder = builder.timeout(Some(std::time::Duration::from_secs(30)));
        if let Some((user, pass)) = &self.credentials {
            builder =
                builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
                    user.clone(),
                    pass.clone(),
                ));
        }
        Ok(builder.build())
    }
}

fn percent_decode(raw: &str) -> String {
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| [k, v].concat())
        .collect()
}

impl Mailer for SmtpMailer {
    fn send(&self, msg: &MailMessage) -> Result<(), MailError> {
        use lettre::Transport;
        let email = lettre::Message::builder()
            .from(
                msg.from
                    .parse()
                    .map_err(|_| MailError::InvalidRecipient("sender address is invalid".into()))?,
            )
            .to(msg
                .to
                .parse()
                .map_err(|_| MailError::InvalidRecipient("address is invalid".into()))?)
            .subject(msg.subject.clone())
            .body(msg.body_text.clone())
            .map_err(|err| MailError::Transport(format!("smtp build failed: {err}")))?;
        let transport = self.transport()?;
        // Synchronous transport on a multi-thread runtime: the worker (and
        // only the worker) runs here. Panics outside one — sync test drains
        // keep the log backend instead.
        tokio::task::block_in_place(|| transport.send(&email))
            .map_err(|err| MailError::Transport(format!("smtp delivery failed: {err}")))?;
        tracing::info!(
            subject = %msg.subject,
            recipient_domain = %msg.to_domain(),
            "mail delivered (smtp)"
        );
        Ok(())
    }
}

/// Pick the production backend from configuration.
///
/// `None` URL → [`DisabledMailer`] (explicit 503s, never fake success).
/// `Some` URL → real [`SmtpMailer`] delivery; a malformed URL or a missing
/// sender is a hard startup error, never a silent pass.
pub fn mailer_from_config(
    smtp_url: Option<&str>,
    mail_from: Option<&str>,
) -> Result<Arc<dyn Mailer>, String> {
    match smtp_url {
        None => {
            if mail_from.is_some() {
                return Err(
                    "--mail-from is set but --smtp-url is not; mail would never send".to_string(),
                );
            }
            Ok(Arc::new(DisabledMailer))
        }
        Some(url) => {
            let from = mail_from.ok_or_else(|| {
                "--smtp-url is set but --mail-from is missing; refusal address required".to_string()
            })?;
            Ok(Arc::new(SmtpMailer::new(url, from)?))
        }
    }
}

/// HTTP-layer environment for the mail endpoints: the sender address plus
/// whether a provider is configured. The request path only *enqueues*
/// durable mail jobs (delivery happens in the worker, after commit); without
/// a provider the endpoints answer an explicit `503`, never a fake success.
#[derive(Clone)]
pub struct MailEnv {
    pub from: String,
    pub enabled: bool,
}

impl MailEnv {
    pub fn new(from: &str, enabled: bool) -> Self {
        Self {
            from: from.to_string(),
            enabled,
        }
    }

    /// Build from server configuration (`--smtp-url` / `mail_from`, or the
    /// JSON file). Validation already ran in `Config::validate`.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            from: cfg.mail_from.clone().unwrap_or_default(),
            enabled: cfg.mail_enabled(),
        }
    }

    /// Explicitly-disabled environment (no provider configured).
    pub fn disabled() -> Self {
        Self {
            from: String::new(),
            enabled: false,
        }
    }
}

/// Startup predicate: signup is possible but no mail provider is configured,
/// so a new account's `SendConfirmationEmail` job could never deliver and the
/// user could never receive their token.
///
/// Signup is possible through open registrations OR through an invite
/// (`register_account_with_invite` enqueues the confirmation job on both
/// paths, even when registrations are closed), so both flags are covered.
/// `true` means "would strand confirmation mail".
#[must_use]
pub fn signup_without_mail(cfg: &Config) -> bool {
    (cfg.registration_open || cfg.invites_enabled) && !cfg.mail_enabled()
}

/// Startup predicate: registrations are open but no mail provider is
/// configured, so a new account's `SendConfirmationEmail` job could never
/// deliver and the user could never receive their token.
///
/// Kept for the open-registration case; prefer [`signup_without_mail`],
/// which additionally covers invite signup while registrations are closed.
/// The job itself still fails loudly (retry, never `Ok`) when this slips
/// through; this predicate lets startup (and the registration endpoint) fail
/// closed instead. `true` means "would strand confirmation mail".
#[must_use]
pub fn open_registration_without_mail(cfg: &Config) -> bool {
    cfg.registration_open && !cfg.mail_enabled()
}

/// Production startup guard for signup without a mail provider.
///
/// Returns `Err` (fatal, with an operator-actionable message naming only the
/// flags, never any secret) when production allows signup — open
/// registrations or enabled invites — and no SMTP provider is configured.
/// Lab passes: the caller logs a warning and the mail job stays explicitly
/// undelivered (retry, never `Ok`).
pub fn require_mail_for_open_registration(cfg: &Config) -> Result<(), String> {
    if signup_without_mail(cfg) && cfg.mode == RuntimeMode::Production {
        return Err("signups are possible but no SMTP provider is configured \
             (--smtp-url and --mail-from); confirmation mail could never deliver: \
             configure SMTP, close registrations and disable invites"
            .to_string());
    }
    Ok(())
}

/// Whether a legacy-seal failure must fail startup.
///
/// `seal_legacy_secrets` / `seal_legacy_actor_keys` only warn today, which
/// lets production keep serving with legacy-plaintext rows and no operator
/// signal. In production the failure is fatal (fail closed: plaintext
/// long-lived secrets must not survive silently); lab only warns. Takes the
/// deployment mode rather than a bare boolean so call sites stay readable.
#[must_use]
pub fn seal_failure_is_fatal(mode: RuntimeMode) -> bool {
    mode == RuntimeMode::Production
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_backend_queues_without_logging_pii() {
        let mailer = LogMailer::new();
        let id = Id::new(7);
        mailer
            .send_confirmation(
                "no-reply@example.social",
                "user@example.com",
                id,
                "secret-token",
                "https://example.social",
            )
            .expect("queue");
        let out = mailer.outbox();
        assert_eq!(out.len(), 1);
        assert!(out[0].body_text.contains("secret-token"));
        assert_eq!(out[0].to_domain(), "example.com");
    }

    #[test]
    fn disabled_backend_refuses_explicitly() {
        let mailer = DisabledMailer;
        let err = mailer
            .send(&MailMessage {
                from: "a".into(),
                to: "b@example.com".into(),
                subject: "s".into(),
                body_text: "b".into(),
            })
            .expect_err("disabled must fail");
        assert!(matches!(err, MailError::Disabled(_)));
    }

    #[test]
    fn config_without_smtp_is_disabled_not_fake_success() {
        let mailer = mailer_from_config(None, None).expect("builds");
        assert!(
            mailer
                .send_confirmation("f", "u@example.com", Id::new(1), "t", "https://x")
                .is_err()
        );
    }

    #[test]
    fn config_rejects_misconfiguration_hard() {
        assert!(mailer_from_config(None, Some("a@b.com")).is_err());
        assert!(mailer_from_config(Some("http://x"), Some("a@b.com")).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), None).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), Some("not-an-address")).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), Some("a@b.com")).is_ok());
        // Shapes beyond host/port/credentials are refused at construction.
        assert!(mailer_from_config(Some("smtp://mail:25?tls=off"), Some("a@b.com")).is_err());
        assert!(mailer_from_config(Some("smtp:///no-host"), Some("a@b.com")).is_err());
        // Credential and port parsing.
        let m = mailer_from_config(Some("smtp://u:p@mail.example:2525"), Some("a@b.com"));
        assert!(m.is_ok());
    }
}
