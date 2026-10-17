use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct Block {
    pub blocker_id: Id,
    pub blocked_id: Id,
}
