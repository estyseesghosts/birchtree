use std::sync::Arc;

use crate::application::streaming::StreamEvent;

use super::account_store::AccountStore;
use super::admin_infra_store::AdminInfraStore;
use super::admin_store::AdminStore;
use super::annual_report_store::AnnualReportStore;
use super::collection_store::CollectionStore;
use super::emoji_store::EmojiStore;
use super::filter_store::FilterStore;
use super::interaction_log::InteractionLog;
use super::list_store::ListStore;
use super::marker_store::MarkerStore;
use super::media_store::MediaStore;
use super::notification_store::NotificationStore;
use super::oauth_store::OAuthStore;
use super::place_store::PlaceStore;
use super::poll_store::PollStore;
use super::quote_store::QuoteStore;
use super::relationship_store::RelationshipStore;
use super::scheduled_store::ScheduledStore;
use super::search_store::SearchStore;
use super::session_store::{CacheStore, JobQueue, ReadyCheck, SessionStore};
use super::status_store::StatusStore;
use super::suggestion_store::SuggestionStore;
use super::timeline_store::TimelineStore;
use super::trend_store::TrendStore;
use super::web_store::WebStore;

// Re-export the two kernel ports under their canonical names.
pub use crate::domain::{Clock, IdGenerator};

/// Bundled persistence surface assembled into a `Context`.
///
/// Implemented once by the `redb` adapter; application code depends on the
/// individual narrow traits above, never on `redb` types.
pub trait Store:
    AccountStore
    + OAuthStore
    + SessionStore
    + JobQueue
    + ReadyCheck
    + InteractionLog
    + StatusStore
    + RelationshipStore
    + PollStore
    + QuoteStore
    + NotificationStore
    + MediaStore
    + PlaceStore
    + ListStore
    + MarkerStore
    + SuggestionStore
    + FilterStore
    + ScheduledStore
    + EmojiStore
    + TimelineStore
    + SearchStore
    + TrendStore
    + AnnualReportStore
    + crate::application::federation_ports::FederationStore
    + crate::application::federation_ports::FederationContentStore
    + AdminInfraStore
    + AdminStore
    + crate::application::announcements::AnnouncementStore
    + crate::application::conversations::ConversationStore
    + crate::application::push::PushSubscriptionStore
    + WebStore
    + CacheStore
    + crate::application::accounts::SecondFactorStore
    + CollectionStore
{
}
impl<T> Store for T where
    T: AccountStore
        + OAuthStore
        + SessionStore
        + JobQueue
        + ReadyCheck
        + InteractionLog
        + StatusStore
        + RelationshipStore
        + PollStore
        + QuoteStore
        + NotificationStore
        + MediaStore
        + PlaceStore
        + ListStore
        + MarkerStore
        + SuggestionStore
        + FilterStore
        + ScheduledStore
        + EmojiStore
        + TimelineStore
        + SearchStore
        + TrendStore
        + AnnualReportStore
        + crate::application::federation_ports::FederationStore
        + crate::application::federation_ports::FederationContentStore
        + AdminInfraStore
        + AdminStore
        + crate::application::announcements::AnnouncementStore
        + crate::application::conversations::ConversationStore
        + crate::application::push::PushSubscriptionStore
        + WebStore
        + CacheStore
        + crate::application::accounts::SecondFactorStore
        + CollectionStore
{
}

/// Publishes committed changes to open streams (Phase 5).
///
/// Non-blocking. The hub's delivery runs on its own thread, so a slow client or
/// a slow policy read never stalls the request that committed the change. If
/// the hub's event queue overflows, the hub closes every open stream instead of
/// dropping the event silently: clients reconnect and reread state.
pub trait StreamingSink: Send + Sync {
    fn publish(&self, event: StreamEvent);
}

/// A sink that discards events. Used where no streaming hub is assembled.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStreaming;

impl StreamingSink for NoStreaming {
    fn publish(&self, _event: StreamEvent) {}
}

/// Snapshot of kernel dependencies assembled in `application::context`.
#[derive(Clone)]
pub struct ContextDeps {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

/// Dependency-assembly surface for adapters.
pub trait Ports: Send + Sync {
    fn clock(&self) -> Arc<dyn Clock>;
    fn ids(&self) -> Arc<dyn IdGenerator>;
}

impl Ports for ContextDeps {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.ids.clone()
    }
}
