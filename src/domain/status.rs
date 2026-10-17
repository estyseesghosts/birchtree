use crate::domain::{Id, Visibility, QuotePolicy};

/// A status (post). Reblogs and quotes are separate records referencing this.
#[derive(Debug, Clone)]
pub struct Status {
    pub id: Id,
    pub account_id: Id,
    pub uri: String,
    pub source_text: String,
    pub content_html: Option<String>,
    pub created_at: u64,
    pub edited_at: Option<u64>,
    pub deleted_at: Option<u64>,
    pub language: Option<String>,
    pub visibility: Visibility,
    pub sensitive: bool,
    pub spoiler_text: Option<String>,
    pub reply_parent_id: Option<Id>,
    pub reply_account_id: Option<Id>,
    pub reblog_of_id: Option<Id>,
    pub media_ids: Vec<Id>,
    pub poll_id: Option<Id>,
    pub quote_policy: QuotePolicy,
}
