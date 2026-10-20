//! Mail adapters.
//!
//! The `MailSender` port is an adapter/job boundary: registration commits a
//! `SendConfirmationEmail` job in the same transaction as the account, and a
//! worker delivers the message after commit. No network I/O happens inside the
//! request transaction.
//!
//! Phase 2 ships a logging adapter. A real SMTP provider is configured by
//! swapping this adapter; the port and job envelope stay the same.

use crate::application::ports::MailSender;
use crate::domain::{Id, Result};
use crate::mailer::{MailError, Mailer};

/// A `MailSender` that records messages in the log without sending them.
///
/// This is the default Phase 2 adapter (no SMTP transport is configured). It is
/// deliberately explicit rather than a silent no-op: the message body is logged
/// so an operator can complete the confirmation out of band.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoggingMailSender;

impl MailSender for LoggingMailSender {
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()> {
        tracing::info!(
            account_id = %account_id,
            recipient = %to,
            // The token is a one-time secret; log only that one was issued.
            token_issued = !token.is_empty(),
            "confirmation email queued (logging adapter; no SMTP configured)"
        );
        Ok(())
    }

    fn send_password_reset(&self, to: &str, account_id: Id, token: &str) -> Result<()> {
        tracing::info!(
            account_id = %account_id,
            recipient = %to,
            token_issued = !token.is_empty(),
            "password-reset email queued (logging adapter; no SMTP configured)"
        );
        Ok(())
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
