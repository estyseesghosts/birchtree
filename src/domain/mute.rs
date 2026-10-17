use crate::domain::Id;

#[derive(Debug, Clone)]
pub struct Mute {
    pub muter_id: Id,
    pub muted_id: Id,
    pub expire_at: Option<u64>,
    pub notify: bool,
}
