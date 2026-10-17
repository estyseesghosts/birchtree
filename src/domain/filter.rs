use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct Filter {
    pub id: Id,
    pub account_id: Id,
    pub phrase: String,
}
