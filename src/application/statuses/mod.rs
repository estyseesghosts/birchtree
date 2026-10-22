//! Status use cases: composing, showing, editing, deleting and reading
//! threads and edit history.
//!
//! Every read goes through the shared visibility policy
//! (`application::policy::can_fetch`) with the viewer/author `PairState`
//! computed by the `RelationshipStore`; policy logic is never re-implemented
//! here. Validation error strings mirror pinned Mastodon v4.7.3 messages
//! (`config/locales/en.yml` and the validators under `app/validators/`).

pub mod create;
pub mod create_mentions;
pub mod create_validate;
pub mod threads;
pub mod update;

pub use self::create::CreateStatusCommand;
pub use self::create::CreateStatusResult;
pub use self::create::PollCommand;
pub use self::create::create_status;
pub use self::create_mentions::canonical_tags;
pub use self::create_mentions::extract_emoji_shortcodes;
pub use self::create_mentions::extract_mention_handles;
pub use self::create_mentions::extract_tags;
pub use self::create_mentions::normalize_tag;
pub use self::create_mentions::normalize_tag_list;
pub use self::create_mentions::render_html;
pub use self::create_mentions::resolve_mentions;
pub use self::create_validate::ABS_MAX_STATUS_LENGTH;
pub use self::create_validate::DEFAULT_MAX_STATUS_LENGTH;
pub use self::create_validate::count_length;
pub use self::create_validate::text_too_long_error;
pub use self::threads::ContextTree;
pub use self::threads::status_context;
pub use self::threads::status_history;
pub use self::update::UpdateStatusCommand;
pub use self::update::account_statuses;
pub use self::update::delete_status;
pub use self::update::show_status;
pub use self::update::status_source;
pub use self::update::statuses_by_ids;
pub use self::update::update_status;
