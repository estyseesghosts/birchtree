//! OAuth scope parsing and validation.
//!
//! Scopes are space-delimited strings. The server's known scope set mirrors
//! Mastodon's `config/initializers/doorkeeper.rb` optional scopes (the admin
//! scopes are accepted for storage but never granted by user-facing flows).
//! A requested scope must be a subset of the application's registered scopes,
//! which is how scope escalation is rejected.

/// The default scope Mastodon grants when none is requested.
pub const DEFAULT_SCOPE: &str = "read";

/// All scopes this build recognizes.
pub const KNOWN_SCOPES: &[&str] = &[
    "profile",
    "read",
    "read:accounts",
    "read:blocks",
    "read:bookmarks",
    "read:collections",
    "read:favourites",
    "read:filters",
    "read:follows",
    "read:lists",
    "read:mutes",
    "read:notifications",
    "read:search",
    "read:statuses",
    "write",
    "write:accounts",
    "write:blocks",
    "write:bookmarks",
    "write:collections",
    "write:conversations",
    "write:favourites",
    "write:filters",
    "write:follows",
    "write:lists",
    "write:media",
    "write:mutes",
    "write:notifications",
    "write:reports",
    "write:statuses",
    "follow",
    "push",
    // Admin API families (Doorkeeper `config/initializers/doorkeeper.rb`).
    // Trends/tags/measures/dimensions/retention use the bare `admin:read` /
    // `admin:write` upstream — no sub-scopes are invented for those here.
    "admin:read",
    "admin:write",
    "admin:read:accounts",
    "admin:write:accounts",
    "admin:read:reports",
    "admin:write:reports",
    "admin:read:domain_allows",
    "admin:write:domain_allows",
    "admin:read:domain_blocks",
    "admin:write:domain_blocks",
    "admin:read:ip_blocks",
    "admin:write:ip_blocks",
    "admin:read:email_domain_blocks",
    "admin:write:email_domain_blocks",
    "admin:read:canonical_email_blocks",
    "admin:write:canonical_email_blocks",
];

/// Split a scope string into a de-duplicated, ordered list.
pub fn parse(scope: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in scope.split_whitespace() {
        let part = part.to_string();
        if !out.contains(&part) {
            out.push(part);
        }
    }
    out
}

/// The default scope set.
pub fn default_scopes() -> Vec<String> {
    vec![DEFAULT_SCOPE.to_string()]
}

/// Whether every scope in `requested` is in `allowed`.
pub fn is_subset(requested: &[String], allowed: &[String]) -> bool {
    requested
        .iter()
        .all(|scope| allowed.iter().any(|a| a == scope))
}

/// Whether a scope string is one this build knows.
pub fn is_known(scope: &str) -> bool {
    KNOWN_SCOPES.contains(&scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dedupes_and_splits() {
        assert_eq!(parse("read write read"), vec!["read", "write"]);
        assert!(parse("  ").is_empty());
    }

    #[test]
    fn subset_detects_escalation() {
        let app = vec!["read".to_string(), "write".to_string()];
        assert!(is_subset(&parse("read"), &app));
        assert!(!is_subset(&parse("read follow"), &app));
    }

    #[test]
    fn known_scopes_include_broad_and_granular() {
        assert!(is_known("read"));
        assert!(is_known("read:accounts"));
        assert!(is_known("write:statuses"));
        assert!(!is_known("read:bogus"));
    }
}
