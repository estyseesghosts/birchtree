//! WebSocket transport (upstream `handleUpgrade`, `onConnection`, `subscribeWebsocketToChannel`,
//! `unsubscribeWebsocketFromChannel`).
//!
//! Authentication happens before the upgrade. A refusal is an HTTP status with
//! an `X-Error-Message` header and an empty body, as upstream writes it. After
//! the upgrade, subscriptions come from the `stream` query parameter and from
//! `{"type":"subscribe"|"unsubscribe","stream":…}` client messages. Refusals of
//! those arrive as `{"error":…,"status":…}` text frames. Event frames are
//! `{"stream":[…],"event":…,"payload":…}`.

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, Uri, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;

use super::StreamError;
use super::auth::{StreamViewer, authenticate, authorize_list, presented_token, require_scopes};
use super::channels::{Channel, Params, resolve, stream_names};
use super::hub::{Frame, Outbox, Session};
use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;

/// Authenticate, register, then upgrade and run the session.
pub async fn serve(state: ApiState, uri: Uri, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let pairs = forms::parse(uri.query().unwrap_or("").as_bytes());
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let presented = presented_token(
        header_text(&headers, header::AUTHORIZATION.as_str()),
        forms::get(&pairs, "access_token"),
        header_text(&headers, "sec-websocket-protocol"),
    );
    let viewer = match authenticate(store, now, presented.as_deref()) {
        Ok(viewer) => viewer,
        Err(err) => return upgrade_refusal(&err),
    };
    let (session, outbox) = match state
        .streams
        .register(viewer.token_id, viewer.viewer, viewer.scopes.clone())
    {
        Ok(opened) => opened,
        Err(err) => return upgrade_refusal(&err),
    };
    // The `stream` query subscription is decided before the upgrade, so an
    // event committed right after the client connects is never missed. Any
    // refusal is sent as the first frame after the upgrade, as upstream does.
    let initial_refusal = forms::get(&pairs, "stream").and_then(|name| {
        let params = Params {
            tag: forms::get(&pairs, "tag").map(str::to_string),
            list: forms::get(&pairs, "list").map(str::to_string),
        };
        subscribe(&state, &viewer, &session, name, &params)
    });
    upgrade.on_upgrade(move |socket| run(socket, state, viewer, session, outbox, initial_refusal))
}

/// Upstream writes the refusal before the upgrade, with `X-Error-Message`.
fn upgrade_refusal(err: &StreamError) -> Response {
    (
        err.status,
        [
            (header::CONNECTION, "close"),
            (header::CONTENT_TYPE, "text/plain"),
            (
                header::HeaderName::from_static("x-error-message"),
                err.message,
            ),
        ],
        "",
    )
        .into_response()
}

/// The next step of the session loop, decided before the select's borrows end.
enum Next {
    Frame(Option<Frame>),
    Incoming(Option<Result<Message, axum::Error>>),
    Ping,
    Closed,
}

async fn run(
    socket: WebSocket,
    state: ApiState,
    viewer: StreamViewer,
    session: Session,
    mut outbox: Outbox,
    initial_refusal: Option<String>,
) {
    let (mut sink, mut incoming) = socket.split();
    if let Some(reply) = initial_refusal {
        if sink.send(Message::Text(reply.into())).await.is_err() {
            return;
        }
    }

    let period = state.streams.limits().ws_ping;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    // Upstream: a connection that did not pong since the last ping is closed.
    let mut alive = true;
    loop {
        let next = tokio::select! {
            frame = outbox.next() => Next::Frame(frame),
            message = incoming.next() => Next::Incoming(message),
            _ = ping.tick() => Next::Ping,
            _ = session.connection().closed() => Next::Closed,
        };
        match next {
            Next::Frame(Some(frame)) => {
                if sink.send(Message::Text(event_text(&frame).into())).await.is_err() {
                    break;
                }
            }
            Next::Frame(None) | Next::Closed => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
            Next::Incoming(Some(Ok(Message::Text(text)))) => {
                if let Some(reply) = client_message(&state, &viewer, &session, &text) {
                    if sink.send(Message::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            }
            Next::Incoming(Some(Ok(Message::Pong(_)))) => alive = true,
            Next::Incoming(Some(Ok(Message::Binary(_)))) => {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame {
                        code: 1003,
                        reason: "The mastodon streaming server does not support binary messages"
                            .into(),
                    })))
                    .await;
                break;
            }
            Next::Incoming(Some(Ok(Message::Close(_))))
            | Next::Incoming(Some(Err(_)))
            | Next::Incoming(None) => break,
            Next::Incoming(Some(Ok(_))) => {}
            Next::Ping => {
                if !alive {
                    break;
                }
                alive = false;
                if sink.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(session);
}

/// One `{"type": …}` client message. Returns an error frame to send back, if any.
fn client_message(state: &ApiState, viewer: &StreamViewer, session: &Session, text: &str) -> Option<String> {
    let Ok(Value::Object(message)) = serde_json::from_str::<Value>(text) else {
        return None;
    };
    let name = match message.get("stream") {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Array(names)) => names
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    let params = Params {
        tag: message.get("tag").and_then(param_text),
        list: message.get("list").and_then(param_text),
    };
    match message.get("type").and_then(Value::as_str) {
        Some("subscribe") => subscribe(state, viewer, session, &name, &params),
        Some("unsubscribe") => unsubscribe(session, &name, &params),
        _ => None,
    }
}

/// Upstream `subscribeWebsocketToChannel`: scopes, then the stream, then the
/// list check. A duplicate subscription is silently a no-op.
fn subscribe(
    state: &ApiState,
    viewer: &StreamViewer,
    session: &Session,
    name: &str,
    params: &Params,
) -> Option<String> {
    let store: &dyn Store = &*state.context.store;
    let channel = match Channel::from_ws(name) {
        Some(channel) => channel,
        None => {
            // Upstream checks the generic scopes before it names the stream.
            let generic = Channel::User;
            if require_scopes(viewer, generic).is_err() {
                return Some(error_frame(&StreamError::unauthorized(
                    "Access token does not have the required scopes",
                )));
            }
            return Some(error_frame(&StreamError::bad_request("Unknown stream type")));
        }
    };
    if let Err(err) = require_scopes(viewer, channel) {
        return Some(error_frame(&err));
    }
    let kind = match resolve(channel, params) {
        Ok(kind) => kind,
        Err(err) => return Some(error_frame(&err)),
    };
    if let Err(err) = authorize_list(store, viewer.viewer, &kind) {
        return Some(error_frame(&err));
    }
    let names = stream_names(name, channel, params);
    match session.subscribe(kind, names) {
        Ok(_) => None,
        Err(err) => Some(error_frame(&err)),
    }
}

/// Upstream `unsubscribeWebsocketFromChannel`: no scope check, one fixed error text.
fn unsubscribe(session: &Session, name: &str, params: &Params) -> Option<String> {
    let outcome = Channel::from_ws(name)
        .and_then(|channel| resolve(channel, params).ok())
        .map(|kind| session.unsubscribe(&kind));
    match outcome {
        Some(_) => None,
        None => Some(serde_json::json!({ "error": "Error unsubscribing from channel" }).to_string()),
    }
}

/// `{"error": …, "status": …}`, upstream's WebSocket refusal frame.
fn error_frame(err: &StreamError) -> String {
    serde_json::json!({ "error": err.message, "status": err.status.as_u16() }).to_string()
}

/// `{"stream": […], "event": …, "payload": …}`, the field order upstream sends.
fn event_text(frame: &Frame) -> String {
    let stream = serde_json::to_string(&*frame.stream).unwrap_or_else(|_| "[]".into());
    let event = serde_json::to_string(frame.event).unwrap_or_else(|_| "\"\"".into());
    let payload = serde_json::to_string(&*frame.payload).unwrap_or_else(|_| "\"\"".into());
    format!(r#"{{"stream":{stream},"event":{event},"payload":{payload}}}"#)
}

/// A `tag` or `list` parameter sent as a string or a number.
fn param_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
