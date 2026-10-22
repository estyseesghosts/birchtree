//! Application ports (S1 decomposition): one trait per submodule.
//!
//! This file is a re-export shim so existing
//! `use crate::application::ports::Store` paths keep working. New code
//! should import from the submodule (e.g. `ports::status_store::StatusStore`).

pub mod account_store;
pub mod admin_infra_store;
pub mod admin_store;
pub mod annual_report_store;
pub mod collection_store;
pub mod emoji_store;
pub mod filter_store;
pub mod interaction_log;
pub mod list_store;
pub mod marker_store;
pub mod media_store;
pub mod notification_store;
pub mod oauth_store;
pub mod place_store;
pub mod poll_store;
pub mod quote_store;
pub mod relationship_store;
pub mod scheduled_store;
pub mod search_store;
pub mod session_store;
pub mod status_store;
pub mod store;
pub mod suggestion_store;
pub mod timeline_store;
pub mod trend_store;
pub mod web_store;

pub use self::account_store::*;
pub use self::admin_infra_store::*;
pub use self::admin_store::*;
pub use self::annual_report_store::*;
pub use self::collection_store::*;
pub use self::emoji_store::*;
pub use self::filter_store::*;
pub use self::interaction_log::*;
pub use self::list_store::*;
pub use self::marker_store::*;
pub use self::media_store::*;
pub use self::notification_store::*;
pub use self::oauth_store::*;
pub use self::place_store::*;
pub use self::poll_store::*;
pub use self::quote_store::*;
pub use self::relationship_store::*;
pub use self::scheduled_store::*;
pub use self::search_store::*;
pub use self::session_store::*;
pub use self::status_store::*;
pub use self::store::*;
pub use self::suggestion_store::*;
pub use self::timeline_store::*;
pub use self::trend_store::*;
pub use self::web_store::*;
