//! Custom filters (Mastodon filter v2 model).
//!
//! Mastodon v4.7.3 stores filters as v2 records (`CustomFilter` with
//! keywords and status rules) and *synthesizes* the v1 API from them (each
//! v1 "filter" is one keyword). One stored representation serves both APIs
//! here, too; `REST::V1::FilterSerializer` projects a keyword row.

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// What a matched filter asks the client to do (`enum :action, {warn: 0,
/// hide: 1, blur: 2}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    #[default]
    Warn,
    Hide,
    Blur,
}

impl FilterAction {
    /// Wire form used by `REST::FilterSerializer#filter_action`.
    pub const fn as_str(self) -> &'static str {
        match self {
            FilterAction::Warn => "warn",
            FilterAction::Hide => "hide",
            FilterAction::Blur => "blur",
        }
    }

    /// Parse the wire value; `None` for anything else (validation errors).
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "warn" => Some(FilterAction::Warn),
            "hide" => Some(FilterAction::Hide),
            "blur" => Some(FilterAction::Blur),
            _ => None,
        }
    }
}

/// A custom filter (v2 record).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFilter {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
    /// `home` / `notifications` / `public` / `account_list`.
    pub context: Vec<String>,
    /// Expiry (ms); `None` = never.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub action: FilterAction,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl CustomFilter {
    /// Mastodon `CustomFilter::TITLE_LENGTH_LIMIT`.
    pub const TITLE_LENGTH_LIMIT: usize = 256;
    /// Mastodon `CustomFilter::VALID_CONTEXTS`.
    pub const VALID_CONTEXTS: [&'static str; 4] = ["home", "notifications", "public", "account_list"];
}

/// One keyword rule of a filter (v1 "filter" row in the synthesized API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterKeyword {
    pub id: Id,
    pub filter_id: Id,
    pub keyword: String,
    #[serde(default)]
    pub whole_word: bool,
}

impl FilterKeyword {
    /// Mastodon `CustomFilterKeyword::KEYWORD_LENGTH_LIMIT`.
    pub const KEYWORD_LENGTH_LIMIT: usize = 500;
}

/// One status rule of a filter (v2-only sub-resource).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterStatus {
    pub id: Id,
    pub filter_id: Id,
    pub status_id: Id,
}
