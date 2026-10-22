use crate::domain::Id;
use crate::domain::Result;

use super::suggestion_store::{FeaturedTag, TagFollow, WebPushSubscription};

/// Phase 7 web/tag rows (Subagent B): settings sync, web push
/// subscriptions, tag follows and featured tags. One `redb` transaction
/// per mutation; reads are bounded scans.
pub trait WebStore: Send + Sync {
    fn web_settings(&self, user_id: Id) -> Result<Option<String>>;
    fn set_web_settings(&self, user_id: Id, data: &str) -> Result<String>;
    fn web_push_subscriptions(&self, user_id: Id) -> Result<Vec<WebPushSubscription>>;
    fn create_web_push_subscription(
        &self,
        user_id: Id,
        endpoint: &str,
        key_p256dh: &str,
        key_auth: &str,
        standard: bool,
        data: &str,
        now_ms: u64,
    ) -> Result<WebPushSubscription>;
    fn update_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
        data: &str,
        now_ms: u64,
    ) -> Result<Option<WebPushSubscription>>;
    fn destroy_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
    ) -> Result<Option<WebPushSubscription>>;
    fn destroy_web_push_subscription_by_token(
        &self,
        token: &str,
    ) -> Result<Option<WebPushSubscription>>;
    fn follow_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<TagFollow>;
    fn unfollow_tag(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>>;
    fn followed_tags(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<TagFollow>>;
    fn tag_follow(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>>;
    fn feature_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<FeaturedTag>;
    fn unfeature_tag(&self, account_id: Id, id: Id) -> Result<Option<FeaturedTag>>;
    fn featured_tags_of(&self, account_id: Id) -> Result<Vec<FeaturedTag>>;
    fn featured_tag(&self, account_id: Id, tag: &str) -> Result<Option<FeaturedTag>>;
}
