//! Hashtag/mention extraction and HTML rendering for statuses.
//!
//! Mention anchors are deliberately not included (the plain text renderer
//! stays deterministic); blank input renders to "" (a media-only status has
//! empty content, as in Mastodon).

use std::sync::OnceLock;

use regex::Regex;

use crate::application::ports::Store;
use crate::domain::Id;
use crate::domain::account::normalize_username;

/// Canonical stored tags for a source text: extract with the D25-widened
/// pattern, [`normalize_tag`] each, drop empties, deduplicate preserving
/// order. Used on create and on every edit (steady-state writes), and by
/// the backfill planner — so the planner converging proves the write path
/// already converged.
pub fn canonical_tags(text: &str) -> Vec<String> {
    normalize_tag_list(&extract_tags(text).into_iter().collect::<Vec<_>>())
}

/// Normalize an explicit tag list (wire `Hashtag` arrays, remote updates):
/// [`normalize_tag`] each, drop empties, deduplicate preserving order.
/// Idempotent — already-normalized input passes through unchanged.
pub fn normalize_tag_list(raw: &[String]) -> Vec<String> {
    let mut tags = Vec::new();
    for item in raw {
        let tag = normalize_tag(item);
        if !tag.is_empty() && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// Extract hashtag bodies after `#` (D25-widened pattern: Unicode
/// letters/digits plus upstream's keep-set), skipping `#` runs that sit
/// inside a word (`café#tag`), preserving order and deduplicating (Mastodon
/// stores unique tags per status). Callers canonicalize with
/// [`normalize_tag`] (see [`canonical_tags`]).
pub fn extract_tags(text: &str) -> Vec<String> {
    let re = tag_regex();
    let mut tags: Vec<String> = Vec::new();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("tag match");
        let before = text[..whole.start()].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let tag = caps.get(1).expect("tag body").as_str().to_ascii_lowercase();
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// Extract mention handles (`@user` or `@user@domain`) that start a line or
/// follow whitespace or `>`, so email addresses are not picked up. Returns
/// the raw handle bodies in order of appearance, deduplicated (case-
/// insensitive dedup happens at id resolution; see [`resolve_mentions`]).
pub fn extract_mention_handles(text: &str) -> Vec<String> {
    let re = mention_regex();
    let mut handles: Vec<String> = Vec::new();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("mention match");
        let before = text[..whole.start()].chars().next_back();
        if !before.is_none_or(|c| c.is_whitespace() || c == '>') {
            continue;
        }
        let handle = caps.get(1).expect("mention body").as_str().to_string();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

/// Resolve mention handles to account ids via
/// `account_by_handle(normalized, domain)`; bare `@user` handles use
/// `local_domain`. Unknown handles are dropped — this is a local-only server
/// (remote phantom accounts are Phase 6) — and duplicate ids are collapsed
/// while preserving mention order.
pub fn resolve_mentions(
    store: &dyn Store,
    handles: &[String],
    local_domain: Option<&str>,
) -> Vec<Id> {
    let mut ids: Vec<Id> = Vec::new();
    for handle in handles {
        let (username, domain) = match handle.split_once('@') {
            Some((user, domain)) => (user, domain),
            None => (handle.as_str(), local_domain.unwrap_or("")),
        };
        if username.is_empty() || domain.is_empty() {
            continue;
        }
        match store.account_by_handle(&normalize_username(username), domain) {
            Ok(Some(account)) => {
                if !ids.contains(&account.id) {
                    ids.push(account.id);
                }
            }
            // Unknown handle or store error: dropped, never phantom-created.
            Ok(None) | Err(_) => {}
        }
    }
    ids
}

/// Render plain source text as sanitized HTML: HTML-escaped, paragraphs
/// split on blank lines, single newlines become `<br />`, wrapped in
/// `<p>...</p>`. Mention anchors are deliberately not included (the plain
/// text renderer stays deterministic); blank input renders to "" (a
/// media-only status has empty content, as in Mastodon).
pub fn render_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }

    let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            if !paragraphs.last().expect("paragraph").is_empty() {
                paragraphs.push(Vec::new());
            }
        } else {
            paragraphs.last_mut().expect("paragraph").push(line);
        }
    }

    let mut html = String::new();
    for paragraph in paragraphs {
        if paragraph.is_empty() {
            continue;
        }
        html.push_str("<p>");
        for (i, line) in paragraph.iter().enumerate() {
            if i > 0 {
                html.push_str("<br />");
            }
            html.push_str(&escape_html(line));
        }
        html.push_str("</p>");
    }
    html
}

/// Escape the five HTML-significant characters (`&` first).
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// URL pattern shared with [`crate::application::statuses::create_validate::count_length`].
pub(crate) fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"https?://\S+").expect("valid URL regex"))
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // D25: the body admits Unicode letters/digits plus upstream's
    // `HASHTAG_INVALID_CHARS_RE` extras; `normalize_tag` canonicalizes each
    // capture at write time (so `#café` stores as `cafe`).
    RE.get_or_init(|| {
        Regex::new(r"#([\p{L}\p{N}_·・\u{200c}\u{0e47}-\u{0e4e}]+)").expect("valid tag regex")
    })
}

fn mention_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"@([A-Za-z0-9_]+(?:@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)?)")
            .expect("valid mention regex")
    })
}

/// Normalize a hashtag wire value the way tags are stored — mirrors Mastodon
/// v4.7.3 `HashtagNormalizer` (`cjk_width` + `lowercase` + `ascii_folding` +
/// `remove_invalid_characters`):
///
/// 1. strip leading `#`/`＃`, apply NFKC *compatibility* folds that matter for
///    hashtags (fullwidth ASCII `Ａ`→`A`, common ligatures `ﬁ`→`fi`, drop
///    combining marks so `e`+`́` and `é` converge);
/// 2. Unicode-lowercase;
/// 3. ASCII-fold accented Latin (`é`→`e`, from upstream's `ASCIIFolding`
///    table), so `#café` and `#cafe` are one tag;
/// 4. drop everything outside letters/digits plus upstream
///    `HASHTAG_INVALID_CHARS_RE`'s extras (`_`, `·`, `・`, ZWNJ, Thai
///    `U+0E47`–`U+0E4E`).
///
/// This is a hand-rolled NFKC *subset*: full Han/emoji compatibility
/// decompositions and halfwidth katakana are out of scope (see
/// `DEPENDENCY-REQUEST` in the Phase 7 report — the `unicode-normalization`
/// crate would replace steps 1–2 with true NFKC). Every behavior the old
/// implementation had (ASCII lowercase + alnum/`_` keep) is preserved.
pub fn normalize_tag(raw: &str) -> String {
    let stripped = raw.trim_start_matches(['#', '＃']);
    let folded = nfkc_compat_fold(stripped);
    ascii_fold(&folded.to_lowercase())
        .chars()
        .filter(|c| {
            c.is_alphanumeric()
                || *c == '_'
                || *c == '·'
                || *c == '・'
                || *c == '\u{200c}'
                || ('\u{0e47}'..='\u{0e4e}').contains(c)
        })
        .collect()
}

/// NFKC compatibility folding, hashtag-scoped subset (see [`normalize_tag`]).
/// Fullwidth ASCII maps to ASCII, the ideographic space is dropped (it would
/// be an invalid tag char anyway), common Latin ligatures expand, and
/// combining marks are stripped so precomposed and decomposed forms converge.
/// Anything unrecognized passes through to the invalid-character filter.
fn nfkc_compat_fold(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        // Fullwidth ASCII `！`..`～` folds to ASCII by a fixed offset.
        if ('\u{ff01}'..='\u{ff5e}').contains(&c) {
            // `char` arithmetic: the offset is a compile-time constant and
            // the range guarantees a valid scalar value.
            let ascii = (c as u32).wrapping_sub(0xfee0);
            out.push(char::from_u32(ascii).unwrap_or(c));
        } else if c == '\u{3000}' {
            // Ideographic space -> space -> dropped as invalid.
        } else if ('\u{0300}'..='\u{036f}').contains(&c)
            || ('\u{1ab0}'..='\u{1aff}').contains(&c)
            || ('\u{1dc0}'..='\u{1dff}').contains(&c)
            || ('\u{20d0}'..='\u{20ff}').contains(&c)
            || ('\u{fe20}'..='\u{fe2f}').contains(&c)
        {
            // Combining mark: stripped.
        } else {
            match c {
                'ﬁ' => out.push_str("fi"),
                'ﬂ' => out.push_str("fl"),
                'ﬀ' => out.push_str("ff"),
                'ﬃ' => out.push_str("ffi"),
                'ﬄ' => out.push_str("ffl"),
                'ﬅ' => out.push_str("ft"),
                'ﬆ' => out.push_str("st"),
                _ => out.push(c),
            }
        }
    }
    out
}

/// Upstream `ASCIIFolding`: accented Latin maps to plain ASCII so tags that
/// differ only by diacritics converge. Table copied from Mastodon v4.7.3
/// `app/lib/ascii_folding.rb`.
fn ascii_fold(input: &str) -> String {
    const FROM: &str = "ÀÁÂÃÄÅàáâãäåĀāĂăĄąÇçĆćĈĉĊċČčÐðĎďĐđÈÉÊËèéêëĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĦħÌÍÎÏìíîïĨĩĪīĬĭĮįİıĴĵĶķĸĹĺĻļĽľĿŀŁłÑñŃńŅņŇňŉŊŋÒÓÔÕÖØòóôõöøŌōŎŏŐőŔŕŖŗŘřŚśŜŝŞşŠšſŢţŤťŦŧÙÚÛÜùúûüŨũŪūŬŭŮůŰűŲųŴŵÝýÿŶŷŸŹźŻżŽž";
    const TO: &str = "AAAAAAaaaaaaAaAaAaCcCcCcCcCcDdDdDdEEEEeeeeEeEeEeEeEeGgGgGgGgHhHhIIIIiiiiIiIiIiIiIiJjKkkLlLlLlLlLlNnNnNnNnnNnOOOOOOooooooOoOoOoRrRrRrSsSsSsSssTtTtTtUUUUuuuuUuUuUuUuUuUuWwYyyYyYZzZzZz";
    input
        .chars()
        .map(|c| {
            FROM.chars()
                .zip(TO.chars())
                .find_map(|(from, to)| (from == c).then_some(to))
                .unwrap_or(c)
        })
        .collect()
}

/// Custom-emoji shortcode tokens (`:shortcode:`) referenced by a text.
///
/// Mirrors `CustomEmoji.from_text`: the token list drives the payload's
/// `emojis` array; the text itself keeps the literal tokens (clients
/// substitute them).
pub fn extract_emoji_shortcodes(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r":([A-Za-z0-9_]{2,}):").expect("valid emoji shortcode regex"));
    re.captures_iter(text)
        .map(|cap| cap[1].to_string())
        .collect()
}

/// BCP 47 language-tag pattern shared with the create/update validators.
pub(crate) fn language_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\A[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*\z").expect("valid language regex")
    })
}
