use crate::domain::{CustomFilter, FilterKeyword, FilterStatus, Id, Result};

/// A filter to create (v2 record; v1 create wraps exactly one keyword).
#[derive(Debug, Clone)]
pub struct NewFilter {
    pub account_id: Id,
    pub title: String,
    pub context: Vec<String>,
    pub expires_at_ms: Option<u64>,
    pub action: crate::domain::FilterAction,
    /// Initial keywords (`v2 keywords_attributes` or the v1 `phrase`).
    pub keywords: Vec<(String, bool)>,
}

/// Field-level filter patch; `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct FilterPatch {
    pub title: Option<String>,
    pub context: Option<Vec<String>>,
    /// `Some(None)` clears the expiry.
    pub expires_at_ms: Option<Option<u64>>,
    pub action: Option<crate::domain::FilterAction>,
    /// Replace keywords wholesale (`v2 keywords_attributes`): `(id, keyword,
    /// whole_word, destroy)`.
    pub keywords: Option<Vec<(Option<Id>, String, bool, bool)>>,
}

/// A filter with its rules, as every filter read returns it.
#[derive(Debug, Clone)]
pub struct FilterBundle {
    pub filter: CustomFilter,
    pub keywords: Vec<FilterKeyword>,
    pub statuses: Vec<FilterStatus>,
}

pub trait FilterStore: Send + Sync {
    fn create_filter(&self, input: NewFilter, now_ms: u64) -> Result<FilterBundle>;
    fn filter_by_id(&self, id: Id) -> Result<Option<FilterBundle>>;
    /// The account's filters (optionally narrowed to a context), bounded.
    fn filters_by_account(
        &self,
        account_id: Id,
        context: Option<&str>,
    ) -> Result<Vec<FilterBundle>>;
    /// Patch an owned filter; `None` when missing/foreign.
    fn update_filter(
        &self,
        account_id: Id,
        id: Id,
        patch: FilterPatch,
        now_ms: u64,
    ) -> Result<Option<FilterBundle>>;
    fn delete_filter(&self, account_id: Id, id: Id) -> Result<bool>;

    fn keyword_by_id(&self, id: Id) -> Result<Option<FilterKeyword>>;
    /// Add a keyword to an owned filter (id = the v1 "filter" id).
    fn create_keyword(
        &self,
        account_id: Id,
        filter_id: Id,
        keyword: &str,
        whole_word: bool,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn update_keyword(
        &self,
        account_id: Id,
        id: Id,
        keyword: Option<&str>,
        whole_word: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn delete_keyword(&self, account_id: Id, id: Id) -> Result<Option<FilterKeyword>>;

    fn filter_statuses(&self, filter_id: Id) -> Result<Vec<FilterStatus>>;
    fn create_filter_status(
        &self,
        account_id: Id,
        filter_id: Id,
        status_id: Id,
        now_ms: u64,
    ) -> Result<Option<FilterStatus>>;
    fn delete_filter_status(&self, account_id: Id, id: Id) -> Result<Option<FilterStatus>>;
}
