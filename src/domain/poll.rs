use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct Poll {
    pub id: Id,
    pub status_id: Id,
    pub multiple: bool,
    pub hide_totals: bool,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PollOption {
    pub id: Id,
    pub poll_id: Id,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct PollVote {
    pub poll_id: Id,
    pub option_id: Id,
    pub account_id: Id,
}
