//! Filter endpoints (Phase 4): v1 (synthesized from v2) + v2 CRUD.
//!
//! Implemented by the filters slice; the routes are registered already so
//! the surface never silently 404s while the handlers land.

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;

macro_rules! stub {
    ($($name:ident),* $(,)?) => {
        $(pub async fn $name(_state: State<ApiState>) -> Response {
            ApiError::internal().into_response()
        })*
    };
}

stub! {
    v1_index,
    v1_create,
    v1_show,
    v1_update,
    v1_destroy,
    v2_index,
    v2_create,
    v2_show,
    v2_update,
    v2_destroy,
    keywords_index,
    keywords_create,
    keywords_show,
    keywords_update,
    keywords_destroy,
    statuses_index,
    statuses_create,
    statuses_show,
    statuses_destroy,
}
