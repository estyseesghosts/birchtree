use crate::domain::{Id, Result, Status};

/// Filters a candidate status through the *viewer's* perspective while a
/// bounded timeline scan fills a page. Implemented in
/// `application::timelines` (the policy stays in the application layer); the
/// store only applies it inside its scan budget.
pub trait TimelineFilter: Send + Sync {
    fn include(&self, status: &Status) -> bool;
}

/// Cursor-bounded timeline query (limit is already endpoint-bounded).
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// `only_media` (public/tag timelines).
    pub only_media: bool,
    /// `local=true` (public/tag timelines).
    pub local_only: bool,
    /// `remote=true` (public/tag timelines).
    pub remote_only: bool,
}

/// One page of a timeline plus the number of index entries the bounded scan
/// walked (executed evidence for the no-unbounded-scan tests).
#[derive(Debug, Clone, Default)]
pub struct TimelinePage {
    pub statuses: Vec<Status>,
    pub scanned: usize,
}

pub trait TimelineStore: Send + Sync {
    /// Public timeline: `public`-visibility statuses, newest first. Replies
    /// (other-than-self) and reblogs are excluded unless the flags say
    /// otherwise (tag timelines include both).
    fn public_timeline(
        &self,
        query: TimelineQuery,
        include_replies: bool,
        include_reblogs: bool,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Hashtag timeline for an already-normalized tag.
    fn hashtag_timeline(
        &self,
        tag: &str,
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Fan-out-on-read merge over `authors` (own + followed accounts for
    /// home; list members for a list), newest first, with per-author and
    /// total scan budgets.
    fn authors_timeline(
        &self,
        authors: &[Id],
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;
}
