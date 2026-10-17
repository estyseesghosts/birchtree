use crate::domain::Id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationType {
    Mention,
    Follow,
    Reblog,
    Favourite,
    PollVote,
    FollowRequest,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: Id,
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub read: bool,
}
