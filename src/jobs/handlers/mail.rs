//! Mail job handlers (S8 slice group B).
//!
//! Confirmation and password-reset delivery. The bearer is minted and
//! persisted (as a digest) in the transaction that queued the job; the
//! handler only reveals and mails it, so a missing or undecryptable token is
//! a retryable error, never a mailed guess.

use crate::application::ports::{MailSender, Store};
use crate::domain::{Error, Id, Job, JobKind, Result};

/// Handle a mail job with the worker context's store and mailer.
///
/// Mirrors `handlers::handle_with_transcode` for
/// [`JobKind::SendConfirmationEmail`] and
/// [`JobKind::SendPasswordResetEmail`]; any other kind is a caller error.
pub fn handle(job: &Job, store: &dyn Store, mailer: &dyn MailSender) -> Result<()> {
    match job.kind {
        JobKind::SendConfirmationEmail => {
            let (account_id, email, token) =
                mail_job_fields(job, &*store, "send_confirmation_email")?;
            mailer.send_confirmation(email, account_id, &token)
        }
        JobKind::SendPasswordResetEmail => {
            let (account_id, email, token) =
                mail_job_fields(job, &*store, "send_password_reset_email")?;
            mailer.send_password_reset(email, account_id, &token)
        }
        _ => Err(Error::Internal("mail: unexpected job kind".into())),
    }
}

/// Read `{account_id, email, token}` from a mail job and reveal the bearer.
///
/// Verbatim copy of `handlers::mail_job_fields`: the token is minted and
/// persisted in the enqueueing transaction, so there is no fallback
/// derivation. A missing field, key, or failed authentication errors so the
/// job retries and quarantines instead of mailing something unredeemable.
fn mail_job_fields<'job>(
    job: &'job Job,
    store: &dyn Store,
    label: &str,
) -> Result<(Id, &'job str, String)> {
    let account_id: Id = job
        .payload
        .get("account_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| Error::Internal(format!("{label}: missing account_id")))?;
    let email: &str = job
        .payload
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Internal(format!("{label}: missing email")))?;
    let stored: &str = job
        .payload
        .get("token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Error::Internal(format!("{label}: missing token")))?;
    let token: String = store.reveal_mail_token(account_id, stored)?;
    Ok((account_id, email, token))
}
