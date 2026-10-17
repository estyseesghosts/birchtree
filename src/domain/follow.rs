use crate::domain::{Id, relationship::RelationshipKind};

/// A follow relationship row.
#[derive(Debug, Clone)]
pub struct Follow {
    pub follower_id: Id,
    pub followed_id: Id,
    pub show_reblogs: bool,
    pub notify: bool,
    pub kind: RelationshipKind,
}
