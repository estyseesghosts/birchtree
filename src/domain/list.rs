use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct List {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct ListMembership {
    pub list_id: Id,
    pub account_id: Id,
}
