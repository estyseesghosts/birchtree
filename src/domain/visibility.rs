use serde::{Deserialize, Serialize};

/// Mastodon visibility levels exposed by the public API.
///
/// `limited` is an internal moderation/restricted state and is projected as
/// `private`; it is never user-creatable and never a fifth API value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
    Direct,
}

impl Visibility {
    /// The wire form (`REST::StatusSerializer#visibility`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Unlisted => "unlisted",
            Visibility::Private => "private",
            Visibility::Direct => "direct",
        }
    }

    /// Parse the wire form; `None` for unknown values.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "public" => Some(Visibility::Public),
            "unlisted" => Some(Visibility::Unlisted),
            "private" => Some(Visibility::Private),
            "direct" => Some(Visibility::Direct),
            _ => None,
        }
    }

    /// Whether the status may appear in public distributable surfaces.
    pub const fn is_distributable(self) -> bool {
        matches!(self, Visibility::Public | Visibility::Unlisted)
    }

    /// Reblogs may not be direct; Mastodon's `selectable_visibilities`.
    pub const fn is_rebloggable(self) -> bool {
        matches!(
            self,
            Visibility::Public | Visibility::Unlisted | Visibility::Private
        )
    }
}

/// Quote policy for a status (Mastodon v4.7.3 `quote_approval_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotePolicy {
    Public,
    Followers,
    Nobody,
}

impl Default for Visibility {
    fn default() -> Self {
        Visibility::Public
    }
}

impl QuotePolicy {
    /// The wire form (`quote_approval_policy` param/serializer values).
    pub const fn as_str(self) -> &'static str {
        match self {
            QuotePolicy::Public => "public",
            QuotePolicy::Followers => "followers",
            QuotePolicy::Nobody => "nobody",
        }
    }

    /// Parse the wire form; `None` for unknown values (API answers 422).
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "public" => Some(QuotePolicy::Public),
            "followers" => Some(QuotePolicy::Followers),
            "nobody" => Some(QuotePolicy::Nobody),
            _ => None,
        }
    }
}

impl Default for QuotePolicy {
    fn default() -> Self {
        QuotePolicy::Public
    }
}
