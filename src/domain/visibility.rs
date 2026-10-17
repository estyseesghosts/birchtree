/// Mastodon visibility levels exposed by the public API.
///
/// `limited` is an internal moderation/restricted state and is projected as
/// `private`; it is never user-creatable and never a fifth API value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
    Direct,
}

/// Quote policy for a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotePolicy {
    Public,
    Followers,
    Nobody,
}

impl Default for QuotePolicy {
    fn default() -> Self {
        QuotePolicy::Public
    }
}
