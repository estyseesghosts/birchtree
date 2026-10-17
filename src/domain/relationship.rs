
/// Direction of a relationship row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// account A follows account B (A -> B)
    Outgoing,
    /// account B is followed by account A (B <- A)
    Incoming,
}

/// Relationship kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipKind {
    Follow,
    FollowRequest,
    Block,
    DomainBlock,
    Mute,
    Favourite,
    Bookmark,
    Pin,
}
