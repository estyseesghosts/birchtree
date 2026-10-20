//! Status visibility and interaction policy.
//!
//! One policy module feeds every read surface (REST show/context/lists,
//! later timelines, search, streaming, quote previews) and every interaction
//! gate. `PairState` is the viewer/author relationship input computed by the
//! `RelationshipStore`.

use crate::application::ports::{PairState, Store};
use crate::domain::{Error, Id, QuotePolicy, Result, Status, Visibility};

/// Whether the viewer is mentioned by the status (visible or silent mentions).
pub fn is_mentioned(viewer: Option<Id>, status: &Status) -> bool {
    match viewer {
        Some(viewer) => {
            status.mentions.contains(&viewer) || status.silent_mentions.contains(&viewer)
        }
        None => false,
    }
}

/// Viewer-side facts that `PairState` does not carry (mute expiry and domain
/// blocks live outside the account-pair tables). Resolved from the store by
/// [`viewer_side_facts`]; [`can_fetch_full`] folds them into the
/// show-equivalent gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchFacts {
    /// The viewer mutes the author (unexpired).
    pub viewer_mutes_author: bool,
    /// The viewer domain-blocks the author's domain.
    pub viewer_domain_blocks_author: bool,
    /// The author domain-blocks the viewer's domain (remote viewers only;
    /// upstream `author_blocking_domain?` is false for local viewers whose
    /// domain is nil — `status_policy.rb:62-66` at pin `8ff11c2b`).
    pub author_domain_blocks_viewer: bool,
}

impl FetchFacts {
    /// Whether any viewer/domain fact refuses the fetch.
    pub fn denies(self) -> bool {
        self.viewer_mutes_author
            || self.viewer_domain_blocks_author
            || self.author_domain_blocks_viewer
    }
}

/// Page size for the author-domain-block membership scan in
/// [`viewer_side_facts`]. Domain-block lists are tiny; one page suffices.
const DOMAIN_BLOCK_SCAN_LIMIT: usize = 1024;

/// Resolve the [`FetchFacts`] for a viewer/author pair from the store.
///
/// Viewer mute/domain-block come from the viewer's relationship to the
/// author (one `relationships` lookup); author-domain-blocks-viewer scans
/// the author's domain-block list for the viewer's domain and applies only
/// to remote viewers. A viewer never carries facts against themselves.
pub fn viewer_side_facts(
    store: &dyn Store,
    viewer_id: Id,
    author_id: Id,
    now_ms: u64,
) -> Result<FetchFacts> {
    if viewer_id == author_id {
        return Ok(FetchFacts::default());
    }
    let author = store
        .account_by_id(author_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let viewer = store
        .account_by_id(viewer_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let mut facts = FetchFacts::default();
    let rels = store.relationships(viewer_id, &[(author_id, author.domain.clone())], now_ms)?;
    if let Some(rel) = rels.first() {
        facts.viewer_mutes_author = rel.muting;
        facts.viewer_domain_blocks_author = rel.domain_blocking;
    }
    if !viewer.local {
        let blocks = store.domain_blocks(author_id, None, None, DOMAIN_BLOCK_SCAN_LIMIT)?;
        let viewer_domain = viewer.domain.to_ascii_lowercase();
        facts.author_domain_blocks_viewer = blocks
            .iter()
            .any(|b| b.domain.to_ascii_lowercase() == viewer_domain);
    }
    Ok(facts)
}

/// Predicate 1: can this viewer fetch this status?
///
/// Mirrors Mastodon v4.7.3 `StatusPolicy#show?` for local statuses
/// (`app/policies/status_policy.rb:1-30` at pin `8ff11c2b`):
/// deleted -> never; `public`/`unlisted` -> anyone except when the author
/// blocks the viewer; `private` -> author, followers, mentioned; `direct` ->
/// author or mentioned only.
///
/// F2 hardening: the fetch additionally refuses when the VIEWER blocks the
/// author (`PairState::blocked_by`). Upstream `show?` does NOT include the
/// viewer-side check (only `reblog?`/`favourite?`/`quote?` deny on
/// `blocking_author?` — `status_policy.rb:17,21,25` at the pin) and never
/// checks mutes, so this gate is deliberately stricter than upstream.
/// Mute and domain-block facts (likewise stricter than upstream `show?`)
/// fold in via [`can_fetch_full`].
pub fn can_fetch(viewer: Option<Id>, status: &Status, pair: PairState) -> bool {
    if status.is_deleted() {
        return false;
    }
    // Either direction of account block refuses the fetch.
    if pair.blocking || pair.blocked_by {
        return false;
    }
    match status.visibility {
        // Anonymous OK.
        Visibility::Public | Visibility::Unlisted => true,
        Visibility::Private => {
            viewer == Some(status.account_id) || pair.followed_by || is_mentioned(viewer, status)
        }
        Visibility::Direct => viewer == Some(status.account_id) || is_mentioned(viewer, status),
    }
}

/// Predicate 1 with viewer-side facts: [`can_fetch`] plus the mute and
/// domain-block refuses (see [`FetchFacts`]). This is the full
/// show-equivalent gate the fetch paths enforce.
pub fn can_fetch_full(
    viewer: Option<Id>,
    status: &Status,
    pair: PairState,
    facts: FetchFacts,
) -> bool {
    can_fetch(viewer, status, pair) && !facts.denies()
}

/// `StatusPolicy#reblog?` (`status_policy.rb:21` at pin `8ff11c2b`): not
/// direct, private only by owner, fetchable, the author does not block the
/// viewer (`can_fetch` covers the author-side block) and the viewer does
/// not block the author (`!blocking_author?`).
pub fn can_reblog(viewer: Id, status: &Status, pair: PairState) -> bool {
    !pair.blocked_by
        && status.visibility.is_rebloggable()
        && (status.visibility != Visibility::Private || viewer == status.account_id)
        && can_fetch(Some(viewer), status, pair)
}

/// `StatusPolicy#favourite?` (`status_policy.rb:25` at pin `8ff11c2b`) =
/// `show?` plus the viewer-side block check. (Upstream has no mute check
/// here; mutes refuse earlier at the fetch layer via [`can_fetch_full`].)
pub fn can_favourite(viewer: Id, status: &Status, pair: PairState) -> bool {
    !pair.blocked_by && can_fetch(Some(viewer), status, pair)
}

/// Bookmark create is show-gated upstream (the bookmarks controller only
/// authorizes `:show?`); the viewer-side block check here is the F2
/// hardening shared with the other action gates, deliberately stricter
/// than upstream.
pub fn can_bookmark(viewer: Id, status: &Status, pair: PairState) -> bool {
    !pair.blocked_by && can_fetch(Some(viewer), status, pair)
}

/// D22: how the viewer's home feed treats statuses that mention an account
/// the viewer blocks or mutes.
///
/// Upstream `FeedManager#filter_from_home` always drops such statuses from
/// home. Birchtree keeps them by default (operator decision D22) and offers
/// per-user options: show everything, collapse the status behind a
/// content-warning-style fold (rendered by the status DTO), or remove it
/// entirely. Stored on the local user as `home_mention_policy`
/// (`show`/`collapse`/`filter`); unknown or missing values mean `Show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HomeMentionPolicy {
    /// Show everything (current behavior, default).
    #[default]
    Show,
    /// Keep the status; clients fold it (DTO `filtered` entry + CW-style fold).
    Collapse,
    /// Remove the status from home (upstream-like).
    Filter,
}

impl HomeMentionPolicy {
    /// Parse the stored wire value; `None`/unknown means `Show`.
    pub fn from_stored(raw: Option<&str>) -> Self {
        match raw {
            Some("collapse" | "fold") => HomeMentionPolicy::Collapse,
            Some("filter" | "hide" | "remove") => HomeMentionPolicy::Filter,
            _ => HomeMentionPolicy::Show,
        }
    }

    /// Stable wire value for the stored preference.
    pub fn as_str(self) -> &'static str {
        match self {
            HomeMentionPolicy::Show => "show",
            HomeMentionPolicy::Collapse => "collapse",
            HomeMentionPolicy::Filter => "filter",
        }
    }
}

/// D23: upstream `filter_from_home` language rule.
///
/// A follow row may carry `languages` (Mastodon `follow.languages`); when the
/// follow names languages and the status names a language the follow does not
/// list, the status is filtered from home. No follow preference, or a status
/// without a language, is always kept. Comparison is ASCII case-insensitive
/// (stored tags are normalized at write time; see
/// [`normalize_follow_languages`]).
pub fn follow_language_allows(
    follow_languages: Option<&[String]>,
    status_language: Option<&str>,
) -> bool {
    let Some(status_language) = status_language else {
        return true;
    };
    let Some(langs) = follow_languages else {
        return true;
    };
    if langs.is_empty() {
        return true;
    }
    langs
        .iter()
        .any(|l| l.eq_ignore_ascii_case(status_language))
}

/// D21: upstream `User#chosen_languages` rule (`PublicFeed#language_scope`,
/// `streaming/index.js` `chosenLanguages` filter).
///
/// When the viewer set preferred languages, a status passes only when its
/// language is one of them, compared exactly (upstream's SQL `IN` and
/// `indexOf` are exact). A status without a language never passes once the
/// preference is set (SQL `NULL NOT IN (...)`, `indexOf(null) === -1`). No
/// preference, or an empty one, keeps everything.
pub fn chosen_language_allows(
    chosen: Option<&[String]>,
    status_language: Option<&str>,
) -> bool {
    let Some(chosen) = chosen else {
        return true;
    };
    if chosen.is_empty() {
        return true;
    }
    let Some(language) = status_language else {
        return false;
    };
    chosen.iter().any(|l| l == language)
}

/// D21: normalize a chosen-languages preference the way upstream's
/// `normalizes :chosen_languages, with: ->(chosen_languages) {
/// chosen_languages.compact_blank.presence }` does: drop blanks, drop
/// empties, deduplicate order-preserving. An empty result means "unset".
/// Unlike follow languages, the tags are NOT case-folded (upstream stores
/// them verbatim and compares exactly).
pub fn normalize_chosen_languages(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.trim().to_string();
        if tag.is_empty() || out.contains(&tag) {
            continue;
        }
        out.push(tag);
    }
    out
}

/// D23: keep the valid BCP-47-ish language tags (same shape as the status
/// `language` validator), lowercased, deduplicated, order-preserving.
/// Invalid entries are dropped, never stored.
pub fn normalize_follow_languages(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.is_empty() || !is_valid_follow_language(&tag) {
            continue;
        }
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// D23: whether `tag` is a storable follow language (letters/digits/`-`,
/// e.g. `en`, `pt-BR`). Mirrors the status `language` validator.
pub fn is_valid_follow_language(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(first) = parts.next() else {
        return false;
    };
    if !(2..=8).contains(&first.len()) || !first.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    for part in parts {
        if !(1..=8).contains(&part.len()) || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
    }
    true
}

/// How the viewer may quote a status (`quote_policy_for_account`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotePermission {
    /// The viewer is the quoted author (self-quotes always allowed).
    SelfQuote,
    /// Allowed by the status's automatic quote policy.
    Automatic,
    /// Denied (`nobody`, direct, reblog, unfetchable, or blocked).
    Denied,
}

/// Compute the viewer's quote permission (`StatusPolicy#quote?` folded with
/// the status's `quote_approval_policy`; `status_policy.rb:15-18` at pin
/// `8ff11c2b`). The viewer-side block check is `quote?`'s
/// `!blocking_author?`; `can_fetch` already covers the author-side block.
pub fn quote_permission(viewer: Option<Id>, status: &Status, pair: PairState) -> QuotePermission {
    // Direct messages and reblogs are never quotable.
    if status.visibility == Visibility::Direct || status.is_reblog() {
        return QuotePermission::Denied;
    }
    if pair.blocked_by {
        return QuotePermission::Denied;
    }
    if !can_fetch(viewer, status, pair) {
        return QuotePermission::Denied;
    }
    if viewer.is_some_and(|v| v == status.account_id) {
        return QuotePermission::SelfQuote;
    }
    match status.quote_policy {
        QuotePolicy::Public => QuotePermission::Automatic,
        // `followers` = the viewer follows the author.
        QuotePolicy::Followers if pair.followed_by => QuotePermission::Automatic,
        QuotePolicy::Followers | QuotePolicy::Nobody => QuotePermission::Denied,
    }
}

/// `StatusPolicy#update?`/`destroy?`: owned.
pub fn is_owner(viewer: Id, status: &Status) -> bool {
    viewer == status.account_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{QuotePolicy, Status, Visibility};

    fn status(visibility: Visibility, author: u64) -> Status {
        Status {
            id: Id::new(100),
            account_id: Id::new(author),
            uri: String::new(),
            url: String::new(),
            source_text: String::new(),
            content_html: String::new(),
            created_at_ms: 0,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: None,
            visibility,
            sensitive: false,
            spoiler_text: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: None,
            media_ids: Vec::new(),
            poll_id: None,
            quote_policy: QuotePolicy::default(),
            mentions: Vec::new(),
            silent_mentions: Vec::new(),
            tags: Vec::new(),
            application: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        }
    }

    #[test]
    fn public_is_fetchable_by_anyone() {
        let s = status(Visibility::Public, 1);
        assert!(can_fetch(None, &s, PairState::default()));
        assert!(can_fetch(Some(Id::new(2)), &s, PairState::default()));
    }

    #[test]
    fn blocked_viewer_cannot_fetch_public() {
        let s = status(Visibility::Public, 1);
        let pair = PairState {
            blocking: true,
            ..PairState::default()
        };
        assert!(!can_fetch(Some(Id::new(2)), &s, pair));
        assert!(!can_favourite(Id::new(2), &s, pair));
    }

    #[test]
    fn private_requires_follow_or_mention() {
        let s = status(Visibility::Private, 1);
        assert!(!can_fetch(Some(Id::new(2)), &s, PairState::default()));
        let follower = PairState {
            followed_by: true,
            ..PairState::default()
        };
        assert!(can_fetch(Some(Id::new(2)), &s, follower));
        assert!(!can_reblog(Id::new(2), &s, follower));
        assert!(can_reblog(Id::new(1), &s, PairState::default()));
    }

    #[test]
    fn direct_is_author_or_mentioned_only() {
        let mut s = status(Visibility::Direct, 1);
        s.mentions.push(Id::new(3));
        assert!(!can_fetch(Some(Id::new(2)), &s, PairState::default()));
        assert!(can_fetch(Some(Id::new(3)), &s, PairState::default()));
        assert!(!can_reblog(Id::new(3), &s, PairState::default()));
    }

    #[test]
    fn quote_policy_gates_quote_permission() {
        let mut s = status(Visibility::Public, 1);
        s.quote_policy = QuotePolicy::Followers;
        assert_eq!(
            quote_permission(Some(Id::new(1)), &s, PairState::default()),
            QuotePermission::SelfQuote
        );
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, PairState::default()),
            QuotePermission::Denied
        );
        let follows = PairState {
            followed_by: true,
            ..PairState::default()
        };
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, follows),
            QuotePermission::Automatic
        );
        s.quote_policy = QuotePolicy::Nobody;
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, follows),
            QuotePermission::Denied
        );
    }

    #[test]
    fn viewer_blocking_author_denies_fetch_and_actions() {
        let s = status(Visibility::Public, 1);
        let pair = PairState {
            blocked_by: true,
            ..PairState::default()
        };
        assert!(!can_fetch(Some(Id::new(2)), &s, pair));
        assert!(!can_reblog(Id::new(2), &s, pair));
        assert!(!can_favourite(Id::new(2), &s, pair));
        assert!(!can_bookmark(Id::new(2), &s, pair));
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, pair),
            QuotePermission::Denied
        );
    }

    #[test]
    fn fetch_facts_deny_on_mute_or_domain() {
        let s = status(Visibility::Public, 1);
        let pair = PairState::default();
        assert!(can_fetch_full(
            Some(Id::new(2)),
            &s,
            pair,
            FetchFacts::default()
        ));
        for facts in [
            FetchFacts {
                viewer_mutes_author: true,
                ..FetchFacts::default()
            },
            FetchFacts {
                viewer_domain_blocks_author: true,
                ..FetchFacts::default()
            },
            FetchFacts {
                author_domain_blocks_viewer: true,
                ..FetchFacts::default()
            },
        ] {
            assert!(facts.denies());
            assert!(!can_fetch_full(Some(Id::new(2)), &s, pair, facts));
        }
        // Anonymous viewers carry no viewer-side facts.
        assert!(!FetchFacts::default().denies());
    }
}
