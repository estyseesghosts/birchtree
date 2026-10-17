//! SSE transport (upstream `streamToHttp`, `streamHttpEnd`, `subscriptionHeartbeat`).
//!
//! The stream opens with a `:)` comment, then carries `event:`/`data:` frames.
//! A `:thump` comment goes out every heartbeat interval. The response ends when
//! the token is revoked, the connection stalls or overflows, or the client
//! leaves.

use std::convert::Infallible;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use super::auth::{authenticate, authorize_list, presented_token, require_scopes};
use super::channels::{Channel, Params, resolve};
use super::http::{error_response, truthy};
use super::hub::{Outbox, Session};
use super::StreamError;
use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;

/// Serve one SSE stream for `tail` (the path after `/api/v1/streaming/`).
pub async fn serve(state: ApiState, tail: String, uri: Uri, headers: HeaderMap) -> Response {
    let pairs = forms::parse(uri.query().unwrap_or("").as_bytes());
    let only_media = truthy(forms::get(&pairs, "only_media"));
    let Some(channel) = Channel::from_sse(&tail, only_media) else {
        return error_response(StatusCode::BAD_REQUEST, "Unknown channel requested");
    };

    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let presented = presented_token(
        header_text(&headers, header::AUTHORIZATION.as_str()),
        forms::get(&pairs, "access_token"),
        header_text(&headers, "sec-websocket-protocol"),
    );
    let viewer = match authenticate(store, now, presented.as_deref()) {
        Ok(viewer) => viewer,
        Err(err) => return refusal(&err),
    };
    if let Err(err) = require_scopes(&viewer, channel) {
        return refusal(&err);
    }
    let params = Params {
        tag: forms::get(&pairs, "tag").map(str::to_string),
        list: forms::get(&pairs, "list").map(str::to_string),
    };
    let kind = match resolve(channel, &params) {
        Ok(kind) => kind,
        Err(err) => return refusal(&err),
    };
    if let Err(err) = authorize_list(store, viewer.viewer, &kind) {
        return refusal(&err);
    }

    let (session, outbox) = match state
        .streams
        .register(viewer.token_id, viewer.viewer, viewer.scopes.clone())
    {
        Ok(opened) => opened,
        Err(err) => return refusal(&err),
    };
    if let Err(err) = session.subscribe(kind, Vec::new()) {
        return refusal(&err);
    }

    let period = state.streams.limits().sse_heartbeat;
    let stream = futures_util::stream::unfold(
        SseStream {
            outbox,
            session,
            heartbeat: tokio::time::interval_at(tokio::time::Instant::now() + period, period),
            opened: false,
        },
        next_chunk,
    );
    let body: Body = Body::from_stream(stream);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        body,
    )
        .into_response()
}

/// The state behind one SSE response. Dropping it unregisters the connection.
struct SseStream {
    outbox: Outbox,
    session: Session,
    heartbeat: tokio::time::Interval,
    opened: bool,
}

/// What the next chunk is, decided before the select's borrows end.
enum Next {
    Frame(Option<super::hub::Frame>),
    Beat,
    Closed,
}

async fn next_chunk(
    mut st: SseStream,
) -> Option<(Result<Bytes, Infallible>, SseStream)> {
    if !st.opened {
        st.opened = true;
        return Some((Ok(Bytes::from_static(b":)\n")), st));
    }
    let next = tokio::select! {
        frame = st.outbox.next() => Next::Frame(frame),
        _ = st.heartbeat.tick() => Next::Beat,
        _ = st.session.connection().closed() => Next::Closed,
    };
    match next {
        Next::Frame(Some(frame)) => {
            let chunk = format!("event: {}\ndata: {}\n\n", frame.event, frame.payload);
            Some((Ok(Bytes::from(chunk)), st))
        }
        Next::Frame(None) | Next::Closed => None,
        Next::Beat => Some((Ok(Bytes::from_static(b":thump\n\n")), st)),
    }
}

fn refusal(err: &StreamError) -> Response {
    error_response(err.status, err.message)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
