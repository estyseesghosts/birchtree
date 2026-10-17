//! `GET /api/v1/streaming` and `GET /api/v1/streaming/{*any}`: one entry point
//! for both transports and the host redirect (upstream's `Api::V1::StreamingController`
//! and the Node server's routes).

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::streaming::{sse, websocket};

const STREAMING_PREFIX: &str = "/api/v1/streaming";

/// The streaming endpoint. A WebSocket upgrade takes the WebSocket path on any
/// streaming URL (upstream's upgrade handler is path-agnostic); everything else
/// is SSE, keyed by the path tail.
pub async fn endpoint(State(state): State<ApiState>, request: Request) -> Response {
    let (mut parts, _body) = request.into_parts();
    let uri: Uri = parts.uri.clone();
    let headers: HeaderMap = parts.headers.clone();

    if let Some(redirect) = host_redirect(&state, &uri, &headers) {
        return redirect;
    }

    let tail = uri
        .path()
        .strip_prefix(STREAMING_PREFIX)
        .unwrap_or("")
        .trim_matches('/');
    if tail == "health" {
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain"),
                (header::CACHE_CONTROL, "private, no-store"),
            ],
            "OK",
        )
            .into_response();
    }

    match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(upgrade) => websocket::serve(state, uri, headers, upgrade).await,
        Err(_) => sse::serve(state, tail.to_string(), uri, headers).await,
    }
}

/// Upstream's `StreamingController#index`: a request for a different streaming
/// host gets a `301` to that host, keeping the scheme and path. Requests to the
/// streaming host itself are served here.
fn host_redirect(state: &ApiState, uri: &Uri, headers: &HeaderMap) -> Option<Response> {
    let configured = state.server.streaming_host.as_ref()?;
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let scheme = if state.server.public_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    if configured.is_same_as(request_host, scheme) {
        return None;
    }
    let path_and_query = match uri.query() {
        Some(query) => format!("{}?{query}", uri.path()),
        None => uri.path().to_string(),
    };
    let location = configured.location(scheme, &path_and_query);
    let mut response = StatusCode::MOVED_PERMANENTLY.into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Some(response)
}

/// Upstream's `bool` reading of `only_media`.
pub fn truthy(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "f" | "false" | "off"
        ),
    }
}

/// `{"error": ...}` with the refusal's status (upstream's `errorMiddleware`).
pub fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "error": message }).to_string(),
    )
        .into_response()
}
