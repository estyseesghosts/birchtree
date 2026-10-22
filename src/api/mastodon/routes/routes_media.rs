use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{delete, get, patch, post, put};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Media (Phase 4): v1 + v2 upload, per-id CRUD.
        .route("/api/v1/media", post(endpoints::media::create))
        .route("/api/v2/media", post(endpoints::media::create_v2))
        .route("/api/v1/media/:id", get(endpoints::media::show))
        .route("/api/v1/media/:id", put(endpoints::media::update))
        .route("/api/v1/media/:id", patch(endpoints::media::update))
        .route("/api/v1/media/:id", delete(endpoints::media::destroy))
        // Local file serving (Mastodon storage path shapes).
        .route(
            "/system/media_attachments/files/*path",
            get(endpoints::media::serve_file),
        )
        .route(
            "/system/custom_emojis/images/*path",
            get(endpoints::emoji::serve_image),
        )
        // Remote emoji (federation tail): the evictable `remote/emojis/`
        // namespace, served through its own gate with refetch-on-miss.
        .route(
            "/system/remote/emojis/*path",
            get(endpoints::emoji::serve_remote_image),
        )
}
