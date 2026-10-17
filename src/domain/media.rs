use crate::domain::Id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaState {
    Pending,
    Processing,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
pub struct MediaAttachment {
    pub id: Id,
    pub account_id: Id,
    pub r#type: String,
    pub url: Option<String>,
    pub state: MediaState,
    pub description: Option<String>,
    pub blurhash: Option<String>,
}
