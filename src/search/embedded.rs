//! In-process inverted index backing [`SearchMode::Embedded`](super::SearchMode).
//!
//! A tokenized inverted index over indexable status text: each distinct token
//! maps to the ids that contain it. Queries are ANDs over query tokens, where
//! one query token matches when it is a prefix of an indexed token (so
//! `rust` finds `rustaceans`). Results come back newest first (numeric id
//! order, never lexicographic on decimal strings).
//!
//! Pure in-memory derived state: every entry is rebuildable from the durable
//! status rows, and every mutation (`index_status`/`remove_status`) is an
//! idempotent overwrite, so a retried job or a repeated backfill batch can
//! never corrupt it. Reads take the lock once and never mutate.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::RwLock;

use crate::domain::Id;

use super::{MIN_QUERY_TOKEN_LEN, tokenize};

/// In-process inverted index: token -> ids containing it, plus each doc's
/// token set so re-indexing and removal stay exact.
pub struct EmbeddedIndex {
    inner: RwLock<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Ordered so one query token is served by a single bounded prefix range.
    postings: BTreeMap<String, BTreeSet<u64>>,
    /// Distinct tokens per indexed status (for exact removal/re-indexing).
    docs: HashMap<u64, Vec<String>>,
}

impl EmbeddedIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }

    /// Index (or re-index) one status's text. Idempotent: re-indexing the
    /// same id with the same text changes nothing; with new text it replaces
    /// the old tokens exactly.
    pub fn index_status(&self, id: Id, text: &str) {
        let mut tokens = tokenize(text);
        tokens.sort();
        tokens.dedup();
        let mut inner = self.inner.write().expect("search index lock");
        Self::remove_locked(&mut inner, id);
        if tokens.is_empty() {
            return;
        }
        for token in &tokens {
            inner
                .postings
                .entry(token.clone())
                .or_default()
                .insert(id.value());
        }
        inner.docs.insert(id.value(), tokens);
    }

    /// Drop one status from the index. Idempotent (missing ids are a no-op).
    pub fn remove_status(&self, id: Id) {
        let mut inner = self.inner.write().expect("search index lock");
        Self::remove_locked(&mut inner, id);
    }

    /// Drop everything (used by tests and by a from-scratch rebuild).
    pub fn clear(&self) {
        let mut inner = self.inner.write().expect("search index lock");
        inner.postings.clear();
        inner.docs.clear();
    }

    /// Number of indexed statuses.
    pub fn len(&self) -> usize {
        self.inner.read().expect("search index lock").docs.len()
    }

    /// Whether no status is indexed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Candidate ids for `query`, newest first, at most `limit`.
    ///
    /// Every query token (length [`MIN_QUERY_TOKEN_LEN`] or more) must prefix-
    /// match at least one indexed token of the status (AND semantics). Queries
    /// with no usable token, and `limit == 0`, match nothing.
    pub fn search_ids(&self, query: &str, limit: usize) -> Vec<Id> {
        if limit == 0 {
            return Vec::new();
        }
        let mut wanted: Vec<String> = tokenize(query)
            .into_iter()
            // `MIN_QUERY_TOKEN_LEN` counts characters, not bytes (see its
            // contract): a two-CJK-character query is served.
            .filter(|t| t.chars().count() >= MIN_QUERY_TOKEN_LEN)
            .collect();
        wanted.sort();
        wanted.dedup();
        if wanted.is_empty() {
            return Vec::new();
        }
        let inner = self.inner.read().expect("search index lock");
        let mut sets: Vec<BTreeSet<u64>> = Vec::with_capacity(wanted.len());
        for token in &wanted {
            let mut merged: BTreeSet<u64> = BTreeSet::new();
            // Ordered prefix range: stops at the first non-matching token.
            for (indexed, ids) in inner.postings.range(token.clone()..) {
                if !indexed.starts_with(token.as_str()) {
                    break;
                }
                merged.extend(ids.iter().copied());
            }
            if merged.is_empty() {
                // One unsatisfiable token empties the AND.
                return Vec::new();
            }
            sets.push(merged);
        }
        // Intersect smallest first.
        sets.sort_by_key(|s| s.len());
        let mut iter = sets.into_iter();
        let mut acc = iter.next().expect("wanted is non-empty");
        for set in iter {
            acc.retain(|id| set.contains(id));
            if acc.is_empty() {
                return Vec::new();
            }
        }
        acc.into_iter()
            .rev()
            .take(limit)
            .map(Id::new)
            .collect()
    }

    fn remove_locked(inner: &mut Inner, id: Id) {
        let Some(tokens) = inner.docs.remove(&id.value()) else {
            return;
        };
        for token in tokens {
            let empty = match inner.postings.get_mut(&token) {
                Some(ids) => {
                    ids.remove(&id.value());
                    ids.is_empty()
                }
                None => false,
            };
            if empty {
                inner.postings.remove(&token);
            }
        }
    }
}

impl Default for EmbeddedIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed(id: u64, text: &str) -> EmbeddedIndex {
        let index = EmbeddedIndex::new();
        index.index_status(Id::new(id), text);
        index
    }

    #[test]
    fn finds_exact_and_prefix_tokens_newest_first() {
        let index = EmbeddedIndex::new();
        index.index_status(Id::new(3), "hello rustaceans world");
        index.index_status(Id::new(7), "hello world");
        // Prefix: `rust` finds `rustaceans`.
        assert_eq!(
            index.search_ids("rust", 10),
            vec![Id::new(3)],
            "prefix match"
        );
        // Newest first.
        assert_eq!(
            index.search_ids("hello", 10),
            vec![Id::new(7), Id::new(3)]
        );
        // AND: both tokens required.
        assert_eq!(index.search_ids("hello rust", 10), vec![Id::new(3)]);
        // No common doc.
        assert!(index.search_ids("rust zebras", 10).is_empty());
        // Limit respected.
        assert_eq!(index.search_ids("hello", 1), vec![Id::new(7)]);
    }

    #[test]
    fn reindex_replaces_and_remove_drops() {
        let index = indexed(1, "alpha beta");
        assert_eq!(index.search_ids("alpha", 10), vec![Id::new(1)]);
        index.index_status(Id::new(1), "gamma delta");
        assert!(index.search_ids("alpha", 10).is_empty());
        assert_eq!(index.search_ids("gamma", 10), vec![Id::new(1)]);
        assert_eq!(index.len(), 1, "re-indexing does not duplicate the doc");
        index.remove_status(Id::new(1));
        assert!(index.search_ids("gamma", 10).is_empty());
        assert!(index.is_empty());
        // Removing twice is a no-op.
        index.remove_status(Id::new(1));
    }

    #[test]
    fn short_and_empty_queries_match_nothing() {
        let index = indexed(1, "alpha beta");
        assert!(index.search_ids("a", 10).is_empty());
        assert!(index.search_ids("", 10).is_empty());
        assert!(index.search_ids("alpha", 0).is_empty());
    }

    #[test]
    fn empty_text_indexes_nothing_but_tracks_no_doc() {
        let index = EmbeddedIndex::new();
        index.index_status(Id::new(1), "!!!");
        assert!(index.is_empty());
        assert!(index.search_ids("x", 10).is_empty());
    }
}
