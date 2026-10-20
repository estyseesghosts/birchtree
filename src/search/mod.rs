//! Phase 7.2 status-search backends (deviations D6/D7).
//!
//! Status search runs in one of three modes behind a single response
//! contract (`GET /api/v2/search` always returns the same four keys):
//!
//! - [`SearchMode::Embedded`] (default, D6): an in-process tokenized inverted
//!   index ([`EmbeddedIndex`]). Derived state only — it is rebuilt from the
//!   durable `redb` records and never authoritative. Indexing is a post-commit
//!   durable job (create/edit/delete/visibility-change); reads never mutate.
//! - [`SearchMode::Database`]: the Phase 4 interim bounded recent scan over
//!   the store (Pleroma's default-backend equivalent).
//! - [`SearchMode::Off`]: status results are always empty, matching Mastodon
//!   with Elasticsearch disabled.
//!
//! Accounts and hashtags stay on the database path in every mode (upstream's
//! `account_search?` is database-backed too, and hashtag search is a tag
//! prefix scan, not full text). Only statuses switch backends.
//!
//! Index scope follows Pleroma's `indexable?`, tightened: only public and
//! unlisted statuses are indexed. Private and direct statuses are never
//! indexed, so they never leave the database through the index.

use std::sync::OnceLock;

use crate::domain::{Id, Status, Visibility};

pub mod backfill;
pub mod embedded;

pub use backfill::{BACKFILL_DEFAULT_BATCH, BACKFILL_MAX_BATCH, BackfillProgress, backfill_batch};
pub use embedded::EmbeddedIndex;

/// Status-search backend selection (configuration value; default `embedded`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// In-process full-text index. On by default (D6, decided 2026-10-17).
    #[default]
    Embedded,
    /// Bounded recent scan (the Phase 4 interim behavior).
    Database,
    /// Empty status results, matching upstream with no engine.
    Off,
}

impl SearchMode {
    /// Parse a configuration/CLI value (`embedded`/`database`/`off`).
    /// `None` for unknown values (the caller reports the error).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "embedded" => Some(SearchMode::Embedded),
            "database" => Some(SearchMode::Database),
            "off" => Some(SearchMode::Off),
            _ => None,
        }
    }

    /// The stable configuration string for this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            SearchMode::Embedded => "embedded",
            SearchMode::Database => "database",
            SearchMode::Off => "off",
        }
    }
}

/// A status-search backend over already-indexed ids. Read-only: search reads
/// never mutate.
pub trait SearchBackend: Send + Sync {
    /// Candidate status ids for `query`, newest first, at most `limit`.
    /// Callers still load the rows and re-check fetchability (blocks,
    /// visibility, deletion) before serving them.
    fn search_status_ids(&self, query: &str, limit: usize) -> Vec<Id>;
}

impl SearchBackend for EmbeddedIndex {
    fn search_status_ids(&self, query: &str, limit: usize) -> Vec<Id> {
        self.search_ids(query, limit)
    }
}

/// The `off` backend: no candidates, like upstream with no search engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct OffBackend;

impl SearchBackend for OffBackend {
    fn search_status_ids(&self, _query: &str, _limit: usize) -> Vec<Id> {
        Vec::new()
    }
}

/// Whether a status may enter the search index.
///
/// Pleroma's `indexable?`, tightened: public and unlisted only. Private and
/// direct statuses are never indexed. Deleted rows and reblog wrappers carry
/// no searchable text of their own; wrappers are admitted (their text is
/// normally empty, so they match nothing) to keep the job wiring uniform —
/// every create/edit/delete runs the same upsert-or-remove step.
pub fn is_indexable(status: &Status) -> bool {
    if status.is_deleted() {
        return false;
    }
    matches!(
        status.visibility,
        Visibility::Public | Visibility::Unlisted
    )
}

/// The text a status contributes to the index: source text plus the content
/// warning (spoiler), so CW-gated terms stay findable.
pub fn indexable_text(status: &Status) -> String {
    match &status.spoiler_text {
        Some(spoiler) if !spoiler.is_empty() => {
            format!("{}\n{spoiler}", status.source_text)
        }
        _ => status.source_text.clone(),
    }
}

/// Tokenize text into lowercase alphanumeric tokens.
///
/// Splits on anything that is not a Unicode alphanumeric (so `café`,
/// CJK text and `_`-joined names behave sanely), drops tokens longer than
/// [`MAX_TOKEN_LEN`] characters, and skips empties. Query text goes through
/// the same function, so both sides agree on token shapes.
///
/// Length contract: the limit counts Unicode scalar values (`char`s), not
/// bytes and not grapheme clusters — a 64-`é` token (128 bytes in UTF-8) is
/// kept whole, while a 65-character token is dropped even if it fits in
/// fewer bytes than some kept token. Case folding may expand a character
/// (e.g. `İ` → `i̇`); the count applies after folding.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    let mut overlong = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if !overlong {
                for lower in ch.to_lowercase() {
                    current.push(lower);
                    current_chars += 1;
                }
                if current_chars > MAX_TOKEN_LEN {
                    overlong = true;
                    current.clear();
                    current_chars = 0;
                }
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
            current_chars = 0;
        } else {
            overlong = false;
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Longest token the index keeps: [`MAX_TOKEN_LEN`] Unicode scalar values
/// (see [`tokenize`]). Longer runs are dropped, never truncated, so a query
/// for a truncated prefix cannot match a dropped token by accident.
pub const MAX_TOKEN_LEN: usize = 64;

/// Minimum query-token length served by the embedded index. Single characters
/// prefix-match most of the vocabulary, so they are dropped; when nothing
/// remains the query matches nothing (still `200` with empty `statuses`).
///
/// Length contract: like [`MAX_TOKEN_LEN`] this counts Unicode scalar values,
/// not bytes — a two-`é` query token (4 bytes) is served, a one-CJK-character
/// token is not.
pub const MIN_QUERY_TOKEN_LEN: usize = 2;

/// Process-wide embedded index (the single-node server has one).
///
/// Job handlers only receive the store, so incremental indexing targets this
/// instance. Tests that need isolation build their own [`EmbeddedIndex`].
static GLOBAL_INDEX: OnceLock<EmbeddedIndex> = OnceLock::new();

/// The process-wide embedded index, created on first use.
pub fn global() -> &'static EmbeddedIndex {
    GLOBAL_INDEX.get_or_init(EmbeddedIndex::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_and_round_trips() {
        assert_eq!(SearchMode::parse("embedded"), Some(SearchMode::Embedded));
        assert_eq!(SearchMode::parse("Database"), Some(SearchMode::Database));
        assert_eq!(SearchMode::parse(" OFF "), Some(SearchMode::Off));
        assert_eq!(SearchMode::parse("elasticsearch"), None);
        assert_eq!(SearchMode::default(), SearchMode::Embedded);
        assert_eq!(SearchMode::Embedded.as_str(), "embedded");
    }

    #[test]
    fn tokenizer_splits_and_lowercases() {
        assert_eq!(tokenize("Hello, World!"), vec!["hello", "world"]);
        assert_eq!(tokenize("#Rustaceans"), vec!["rustaceans"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
        // Unicode alphanumerics survive; case folds.
        assert_eq!(tokenize("Café"), vec!["café"]);
    }

    #[test]
    fn off_backend_is_empty() {
        assert!(OffBackend.search_status_ids("anything", 10).is_empty());
    }
}
