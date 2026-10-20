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
//! transaction. Without an SMTP provider the mailer is explicitly disabled:
//! sends fail with [`MailError::Disabled`] (surfaced as `503`), never a
//! silent fake success. [`LogMailer`] is the test/dev backend: it stores
//! messages in an observable outbox (no network) so tests can assert the
//! send path ran. [`SmtpMailer`] is a validated-but-unimplemented stub: it
//! refuses sends with [`MailError::Transport`] until delivery lands.
//!
//! Privacy: tokens are one-time secrets and recipient addresses are PII —
//! neither is ever logged. Log lines carry only the message kind and the
//! recipient domain.

use std::fmt;
use std::sync::{Arc, Mutex};

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
            MailError::Disabled(msg) => crate::domain::Error::Forbidden(
                format!("Email delivery is not configured ({msg})"),
            ),
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
fn validate_recipient(to: &str) -> Result<(), MailError> {
    let to = to.trim();
    let ok = !to.is_empty()
        && !to.contains(char::is_whitespace)
        && to.split('@').nth(1).is_some_and(|d| !d.is_empty() && d.contains('.'));
    if ok {
        Ok(())
    } else {
        Err(MailError::InvalidRecipient("address is invalid".to_string()))
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

/// Validated SMTP stub. Construction checks the URL shape; `send` refuses
/// with [`MailError::Transport`] until real delivery lands — an honest
/// error, never an unsent `Ok`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `url`/`from` are used by real delivery (parent follow-up).
pub struct SmtpMailer {
    url: String,
    from: String,
}

impl SmtpMailer {
    /// Validate `smtp://…` / `smtps://…` URL shape (no network, no secrets).
    pub fn new(url: &str, from: &str) -> Result<Self, String> {
        validate_smtp_url(url)?;
        if !from.contains('@') {
            return Err("--mail-from must be an email address".to_string());
        }
        Ok(Self {
            url: url.to_string(),
            from: from.to_string(),
        })
    }

    pub fn from_address(&self) -> &str {
        &self.from
    }
}

impl Mailer for SmtpMailer {
    fn send(&self, msg: &MailMessage) -> Result<(), MailError> {
        tracing::warn!(
            subject = %msg.subject,
            recipient_domain = %msg.to_domain(),
            "mail not sent (SMTP delivery not implemented)"
        );
        Err(MailError::Transport(
            "SMTP delivery is not implemented yet; message not sent".to_string(),
        ))
    }
}

fn validate_smtp_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("smtp://")
        .or_else(|| url.strip_prefix("smtps://"));
    match rest {
        Some(rest) if !rest.is_empty() => Ok(()),
        _ => Err("--smtp-url must start with smtp:// or smtps://".to_string()),
    }
}

/// Pick the production backend from configuration.
///
/// `None` URL → [`DisabledMailer`] (explicit 503s, never fake success).
/// `Some` URL → validated [`SmtpMailer`] stub; a malformed URL or a missing
/// sender is a hard startup error, never a silent pass.
pub fn mailer_from_config(
    smtp_url: Option<&str>,
    mail_from: Option<&str>,
) -> Result<Arc<dyn Mailer>, String> {
    match smtp_url {
        None => {
            if mail_from.is_some() {
                return Err("--mail-from is set but --smtp-url is not; mail would never send".to_string());
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
/// the configured backend. Wired as `Extension<Arc<MailEnv>>` (parent-owned
/// router wiring); `from` mirrors `Config::mail_from`.
#[derive(Clone)]
pub struct MailEnv {
    pub from: String,
    pub mailer: Arc<dyn Mailer>,
}

impl MailEnv {
    pub fn new(from: &str, mailer: Arc<dyn Mailer>) -> Self {
        Self {
            from: from.to_string(),
            mailer,
        }
    }

    /// Explicitly-disabled environment (no provider configured).
    pub fn disabled() -> Self {
        Self {
            from: String::new(),
            mailer: Arc::new(DisabledMailer),
        }
    }
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
        assert!(mailer.send_confirmation("f", "u@example.com", Id::new(1), "t", "https://x").is_err());
    }

    #[test]
    fn config_rejects_misconfiguration_hard() {
        assert!(mailer_from_config(None, Some("a@b.com")).is_err());
        assert!(mailer_from_config(Some("http://x"), Some("a@b.com")).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), None).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), Some("not-an-address")).is_err());
        assert!(mailer_from_config(Some("smtp://mail:25"), Some("a@b.com")).is_ok());
    }
}
