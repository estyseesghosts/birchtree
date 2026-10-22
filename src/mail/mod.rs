//! Mail adapters.
//!
//! The `MailSender` port is an adapter/job boundary: registration commits a
//! `SendConfirmationEmail` job in the same transaction as the account, and a
//! worker delivers the message after commit. No network I/O happens inside the
//! request transaction.
//!
//! When no SMTP provider is configured, sending is explicitly refused: the
//! worker maps the error to retry-then-quarantine (operator-visible), never a
//! silent fake success. Production startup additionally refuses open
//! registrations without a provider (see
//! [`crate::mailer::require_mail_for_open_registration`]); the proposed
//! `main.rs` wiring is in the area report.

use crate::application::ports::MailSender;
use crate::domain::{Error, Id, Result};
use crate::mailer::{MailError, Mailer};

/// Development-only [`MailSender`] that refuses every send.
///
/// There is deliberately no logging-success path: when no SMTP provider is
/// configured, the confirmation job must stay retryable (the worker maps the
/// error to retry, then quarantine) rather than complete as if the user had
/// been mailed. Production startup additionally refuses open registrations
/// without a provider, so this adapter only ever fires on a misconfigured or
/// lab node — loudly.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoggingMailSender;

impl LoggingMailSender {
    /// Recipient domain for log lines (never the full address: PII).
    fn recipient_domain(to: &str) -> &str {
        to.rsplit('@').next().unwrap_or("")
    }
}

/// Refuse a send: warn (domain only, no token, no address) and return a
/// retryable error for the worker.
fn refused(reason: &str, account_id: Id, to: &str) -> Error {
    tracing::warn!(
        account_id = %account_id,
        recipient_domain = %LoggingMailSender::recipient_domain(to),
        reason = %reason,
        "mail not sent: no SMTP provider configured",
    );
    Error::Internal("email delivery is not configured (no SMTP provider)".to_string())
}

impl MailSender for LoggingMailSender {
    fn send_confirmation(&self, to: &str, account_id: Id, _token: &str) -> Result<()> {
        Err(refused("confirmation", account_id, to))
    }

    fn send_password_reset(&self, to: &str, account_id: Id, _token: &str) -> Result<()> {
        Err(refused("password-reset", account_id, to))
    }
}

/// A `MailSender` that delivers through a configured [`Mailer`] backend
/// (production: the real SMTP mailer) with the operator's sender address and
/// public base URL (for the token links). Used by the worker; the request
/// path only enqueues jobs, never sends.
pub struct SmtpJobSender {
    mailer: std::sync::Arc<dyn Mailer>,
    from: String,
    base_url: String,
}

impl SmtpJobSender {
    pub fn new(mailer: std::sync::Arc<dyn Mailer>, from: &str, base_url: &str) -> Self {
        Self {
            mailer,
            from: from.to_string(),
            base_url: base_url.to_string(),
        }
    }
}

impl MailSender for SmtpJobSender {
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()> {
        self.mailer
            .send_confirmation(&self.from, to, account_id, token, &self.base_url)
            .map_err(domain_mail_error)
    }

    fn send_password_reset(&self, to: &str, account_id: Id, token: &str) -> Result<()> {
        self.mailer
            .send_password_reset(&self.from, to, account_id, token, &self.base_url)
            .map_err(domain_mail_error)
    }
}

fn domain_mail_error(err: MailError) -> crate::domain::Error {
    err.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_adapter_never_reports_success() {
        let sender = LoggingMailSender;
        let err = sender
            .send_confirmation("user@example.com", Id::new(1), "secret-token")
            .expect_err("no provider must fail, never fake success");
        assert!(matches!(err, Error::Internal(_)));
        let err = sender
            .send_password_reset("user@example.com", Id::new(1), "secret-token")
            .expect_err("no provider must fail, never fake success");
        assert!(matches!(err, Error::Internal(_)));
    }

    #[test]
    fn recipient_domain_never_carries_the_local_part() {
        assert_eq!(
            LoggingMailSender::recipient_domain("user@example.com"),
            "example.com"
        );
        assert_eq!(
            LoggingMailSender::recipient_domain("no-at-sign"),
            "no-at-sign"
        );
    }
}
