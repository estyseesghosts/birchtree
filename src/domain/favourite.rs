use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct Favourite {
    pub account_id: Id,
    pub status_id: Id,
}
