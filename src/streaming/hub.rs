//! The streaming hub: registry, bounded queues, caps and the sink adapter.
//!
//! Lifecycle of a change: a use case commits, the adapter calls
//! [`StreamingSink::publish`] (non-blocking), and the hub's thread drains the
//! event queue and runs [`fanout::process`]. Each frame is queued on the
//! subscriber's connection. A transport drains that queue to its socket.
//!
//! Bounds (the contract is in `docs/streaming-contract.md`):
//! - The event queue holds `event_queue` events. Overflow closes every open
//!   stream rather than dropping an event silently.
//! - A connection queues at most `queue_frames` frames and `queue_bytes`
//!   bytes. Past either, the connection closes.
//! - A connection whose transport has taken no frame for `stall_timeout`
//!   while frames are queued closes. A stalled client cannot pin memory.
//! - Connections, connections per token and subscriptions per connection are
//!   capped.
//!
//! Policy epochs: a connection's policy epoch rises whenever live policy
//! removes visibility for its viewer. A frame queued under an older epoch is
//! dropped at the transport, so a block, mute, unfollow or domain block is never
//! followed by delivery of frames queued before it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use tokio::sync::{Notify, mpsc as tokio_mpsc};

use super::StreamError;
use super::channels::{StreamKind, channel_ids, subscription_key};
use crate::api::mastodon::ServerInfo;
use crate::application::ports::{Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{Clock, Id};

/// Event-loop tick: the stall sweep runs at least this often.
const SWEEP_INTERVAL: Duration = Duration::from_millis(100);

/// Configured bounds. [`Limits::default`] is the production profile.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Open connections across all tokens.
    pub max_connections: usize,
    /// Open connections for one token.
    pub max_connections_per_token: usize,
    /// Subscriptions on one connection (WebSocket only: SSE has one).
    pub max_subscriptions_per_connection: usize,
    /// Frames queued for one connection.
    pub queue_frames: usize,
    /// Bytes queued for one connection.
    pub queue_bytes: usize,
    /// Events queued between publishers and the fan-out thread.
    pub event_queue: usize,
    /// A connection with queued frames and no transport progress for this long
    /// is closed.
    pub stall_timeout: Duration,
    /// SSE heartbeat (upstream: `:thump` every 15 seconds).
    pub sse_heartbeat: Duration,
    /// WebSocket ping interval (upstream: 30 seconds, closed without a pong).
    pub ws_ping: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 512,
            max_connections_per_token: 16,
            max_subscriptions_per_connection: 16,
            queue_frames: 256,
            queue_bytes: 1024 * 1024,
            event_queue: 4096,
            stall_timeout: Duration::from_secs(30),
            sse_heartbeat: Duration::from_secs(15),
            ws_ping: Duration::from_secs(30),
        }
    }
}

/// A committed announcement change (gaps batch, G-B; closes the streaming
/// half of the announcements TODO).
///
/// Upstream (`PublishAnnouncementReactionWorker`, the publish/delete Rails
/// paths) publishes `announcement`, `announcement.reaction` and
/// `announcement.delete` to `timeline:{account.id}` — i.e. every connected
/// `user` stream — for every active account with a live subscription. The
/// `user:notification` stream (the `:notifications` channel) does NOT carry
/// them. Payloads (docs.joinmastodon.org/methods/streaming):
/// - `announcement`: the Announcement JSON, rendered per subscriber (`read`
///   and reaction `me` flags are per-viewer);
/// - `announcement.reaction`: `{name, count, announcement_id}` (+
///   `url`/`static_url` for custom emoji);
/// - `announcement.delete`: the bare announcement id string (the row is
///   gone, like a status `delete`).
///
/// Dismissals (mutes) publish nothing: read state is per-user, exactly like
/// upstream.
#[derive(Debug, Clone)]
pub enum AnnouncementEvent {
    /// A row was published (admin create with past/no schedule, or explicit
    /// publish). The hub re-reads the row; a missing/unpublished row sends
    /// nothing (a delete raced the publish).
    Published { announcement_id: Id },
    /// A reaction was added or removed. The hub recomputes the count at
    /// delivery (like the upstream worker); a missing row sends nothing.
    Reaction {
        announcement_id: Id,
        name: String,
    },
    /// A row was unpublished or destroyed. Carries the id because the row
    /// may be gone; the payload is the bare id string.
    Deleted { announcement_id: Id },
}

/// One frame for a transport to write.
#[derive(Debug)]
pub struct Frame {
    /// The connection's policy epoch when the frame was queued.
    pub epoch: u64,
    /// WebSocket `stream` array for the subscription that produced it.
    pub stream: Arc<[String]>,
    pub event: &'static str,
    /// Serialized payload (a status or notification JSON, or a status id).
    pub payload: Arc<str>,
}

impl Frame {
    pub fn new(event: &'static str, stream: Arc<[String]>, payload: Arc<str>) -> Self {
        Self {
            epoch: 0,
            stream,
            event,
            payload,
        }
    }

    fn size(&self) -> usize {
        self.event.len()
            + self.payload.len()
            + self.stream.iter().map(String::len).sum::<usize>()
    }
}

/// One open connection.
pub struct Connection {
    pub id: u64,
    pub token_id: Id,
    pub viewer: Id,
    pub scopes: Vec<String>,
    epoch: AtomicU64,
    queued_frames: AtomicUsize,
    queued_bytes: AtomicUsize,
    /// Milliseconds since hub start at the transport's last progress.
    last_progress_ms: AtomicU64,
    closed: AtomicBool,
    /// Wakes the transport when the connection closes. `notify_one` stores a
    /// permit, so a close before the transport waits is not lost.
    closing: Notify,
    tx: tokio_mpsc::Sender<Frame>,
    started: Instant,
}

impl Connection {
    fn new(
        id: u64,
        token_id: Id,
        viewer: Id,
        scopes: Vec<String>,
        capacity: usize,
        started: Instant,
    ) -> (Arc<Self>, tokio_mpsc::Receiver<Frame>) {
        let (tx, rx) = tokio_mpsc::channel(capacity.max(1));
        let conn = Arc::new(Self {
            id,
            token_id,
            viewer,
            scopes,
            epoch: AtomicU64::new(0),
            queued_frames: AtomicUsize::new(0),
            queued_bytes: AtomicUsize::new(0),
            last_progress_ms: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            closing: Notify::new(),
            tx,
            started,
        });
        (conn, rx)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Resolves when the connection closes (a permit is kept if it closed
    /// earlier).
    pub async fn closed(&self) {
        if self.is_closed() {
            return;
        }
        self.closing.notified().await;
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    fn touch(&self) {
        let elapsed = self.started.elapsed().as_millis() as u64;
        self.last_progress_ms.store(elapsed, Ordering::Release);
    }

    /// Queue a frame, stamped with the current policy epoch. `false` when the
    /// queue is full or the connection is closed; the caller closes it.
    fn enqueue(&self, mut frame: Frame, limits: &Limits) -> bool {
        if self.is_closed() {
            return false;
        }
        frame.epoch = self.epoch();
        let size = frame.size();
        if self.queued_bytes.load(Ordering::Acquire) + size > limits.queue_bytes {
            return false;
        }
        if self.tx.try_send(frame).is_err() {
            return false;
        }
        self.queued_frames.fetch_add(1, Ordering::AcqRel);
        self.queued_bytes.fetch_add(size, Ordering::AcqRel);
        true
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.closing.notify_one();
    }
}

/// The transport's side of a connection's queue.
pub struct Outbox {
    rx: tokio_mpsc::Receiver<Frame>,
    conn: Arc<Connection>,
}

impl Outbox {
    /// The next frame that is still valid under the policy epoch. Frames queued
    /// before a policy change are dropped here.
    pub async fn next(&mut self) -> Option<Frame> {
        loop {
            let frame = self.rx.recv().await?;
            self.conn.queued_frames.fetch_sub(1, Ordering::AcqRel);
            self.conn.queued_bytes.fetch_sub(frame.size(), Ordering::AcqRel);
            self.conn.touch();
            if frame.epoch < self.conn.epoch() {
                continue;
            }
            return Some(frame);
        }
    }
}

/// One subscription (a WebSocket stream, or an SSE connection's only stream).
#[derive(Clone)]
pub struct Subscription {
    pub conn_id: u64,
    pub viewer: Id,
    pub kind: StreamKind,
    pub names: Arc<[String]>,
    pub key: String,
    /// Whether the token may read notifications (folds them into `user`).
    pub notifications_scope: bool,
}

#[derive(Default)]
struct Registry {
    conns: HashMap<u64, Arc<Connection>>,
    subs: HashMap<u64, Vec<Subscription>>,
    per_token: HashMap<Id, usize>,
    next_id: u64,
}

/// Shared state. Read by the fan-out thread and by the transports.
pub(crate) struct Core {
    pub limits: Limits,
    pub store: Arc<dyn Store>,
    pub clock: Arc<dyn Clock>,
    pub server: Arc<ServerInfo>,
    started: Instant,
    registry: Mutex<Registry>,
    overflow: AtomicBool,
    dropped_events: AtomicU64,
}

impl Core {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every live (connection, subscription) pair, cloned out of the lock.
    pub(crate) fn subscriptions(&self) -> Vec<(Arc<Connection>, Subscription)> {
        let registry = self.registry();
        registry
            .subs
            .iter()
            .flat_map(|(conn_id, subs)| {
                let conn = registry.conns.get(conn_id).cloned();
                subs.iter()
                    .filter_map(move |sub| conn.clone().map(|c| (c, sub.clone())))
            })
            .collect()
    }

    /// Queue `frame` on `conn`, closing the connection when it cannot take it.
    pub(crate) fn deliver(&self, conn: &Arc<Connection>, frame: Frame) {
        if !conn.enqueue(frame, &self.limits) {
            self.close_connection(conn.id);
        }
    }

    /// Raise the policy epoch of every connection of `viewers`.
    pub(crate) fn bump_epochs(&self, viewers: &[Id]) {
        for conn in self.registry().conns.values() {
            if viewers.contains(&conn.viewer) {
                conn.epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Close every connection of one token (upstream's `kill`).
    pub(crate) fn close_token(&self, token_id: Id) {
        let ids: Vec<u64> = self
            .registry()
            .conns
            .values()
            .filter(|conn| conn.token_id == token_id)
            .map(|conn| conn.id)
            .collect();
        for id in ids {
            self.close_connection(id);
        }
    }

    fn close_all(&self) {
        let ids: Vec<u64> = self.registry().conns.keys().copied().collect();
        for id in ids {
            self.close_connection(id);
        }
    }

    /// Close the connections whose transport has queued frames and no progress
    /// for `stall_timeout`.
    fn sweep_stalled(&self) {
        let now = self.started.elapsed().as_millis() as u64;
        let limit = self.limits.stall_timeout.as_millis() as u64;
        let stalled: Vec<u64> = self
            .registry()
            .conns
            .values()
            .filter(|conn| {
                conn.queued_frames.load(Ordering::Acquire) > 0
                    && now.saturating_sub(conn.last_progress_ms.load(Ordering::Acquire)) > limit
            })
            .map(|conn| conn.id)
            .collect();
        for id in stalled {
            tracing::warn!(connection = id, "closing stalled stream connection");
            self.close_connection(id);
        }
    }

    /// Remove a connection and its subscriptions, and close its transport.
    fn close_connection(&self, id: u64) {
        let removed = {
            let mut registry = self.registry();
            let conn = registry.conns.remove(&id);
            registry.subs.remove(&id);
            if let Some(conn) = &conn {
                if let Some(count) = registry.per_token.get_mut(&conn.token_id) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        registry.per_token.remove(&conn.token_id);
                    }
                }
            }
            conn
        };
        if let Some(conn) = removed {
            conn.close();
        }
    }
}

/// The fan-out thread: drains events, runs the policy, queues frames.
///
/// Announcement events ride a second bounded queue with the same overflow
/// policy (overflow closes every stream rather than dropping an event
/// silently). One thread drains both so frame order per subscription stays
/// commit-ordered per event source.
fn run(core: Arc<Core>, rx: mpsc::Receiver<StreamEvent>, arx: mpsc::Receiver<AnnouncementEvent>) {
    loop {
        match rx.recv_timeout(SWEEP_INTERVAL) {
            Ok(event) => super::fanout::process(&core, event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(announcement) = arx.try_recv() {
            super::fanout::process_announcement(&core, announcement);
        }
        if core.overflow.swap(false, Ordering::AcqRel) {
            tracing::warn!("stream event queue overflowed; closing every stream");
            core.close_all();
        }
        core.sweep_stalled();
    }
}

/// Counters for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub dropped_events: u64,
}

/// A handle on the hub. Cheap to clone; every clone shares one hub.
#[derive(Clone)]
pub struct Hub {
    core: Arc<Core>,
    events: SyncSender<StreamEvent>,
    announcements: SyncSender<AnnouncementEvent>,
}

impl Hub {
    /// Start a hub and its fan-out thread. The thread exits when every handle is
    /// dropped.
    pub fn new(
        store: Arc<dyn Store>,
        clock: Arc<dyn Clock>,
        server: Arc<ServerInfo>,
        limits: Limits,
    ) -> Self {
        let (events, rx) = mpsc::sync_channel(limits.event_queue.max(1));
        let (announcements, arx) = mpsc::sync_channel(limits.event_queue.max(1));
        let core = Arc::new(Core {
            limits,
            store,
            clock,
            server,
            started: Instant::now(),
            registry: Mutex::new(Registry::default()),
            overflow: AtomicBool::new(false),
            dropped_events: AtomicU64::new(0),
        });
        let thread_core = core.clone();
        std::thread::Builder::new()
            .name("birchtree-streams".into())
            .spawn(move || run(thread_core, rx, arx))
            .expect("spawn stream fan-out thread");
        Self { core, events, announcements }
    }

    /// The configured bounds.
    pub fn limits(&self) -> &Limits {
        &self.core.limits
    }

    /// Open a connection for `viewer`. Refused past the total or per-token cap.
    pub fn register(
        &self,
        token_id: Id,
        viewer: Id,
        scopes: Vec<String>,
    ) -> Result<(Session, Outbox), StreamError> {
        let limits = &self.core.limits;
        let mut registry = self.core.registry();
        if registry.conns.len() >= limits.max_connections {
            return Err(StreamError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Too many streaming connections",
            ));
        }
        if registry.per_token.get(&token_id).copied().unwrap_or(0)
            >= limits.max_connections_per_token
        {
            return Err(StreamError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many streaming connections for this token",
            ));
        }
        registry.next_id += 1;
        let id = registry.next_id;
        let (conn, rx) = Connection::new(
            id,
            token_id,
            viewer,
            scopes,
            limits.queue_frames,
            self.core.started,
        );
        conn.touch();
        *registry.per_token.entry(token_id).or_insert(0) += 1;
        registry.conns.insert(id, conn.clone());
        drop(registry);
        let session = Session {
            hub: self.clone(),
            conn: conn.clone(),
        };
        Ok((session, Outbox { rx, conn }))
    }

    /// Counters for tests and diagnostics.
    pub fn stats(&self) -> HubStats {
        let registry = self.core.registry();
        HubStats {
            connections: registry.conns.len(),
            subscriptions: registry.subs.values().map(Vec::len).sum(),
            dropped_events: self.core.dropped_events.load(Ordering::Acquire),
        }
    }

    fn close_connection(&self, id: u64) {
        self.core.close_connection(id);
    }
}

impl StreamingSink for Hub {
    fn publish(&self, event: StreamEvent) {
        match self.events.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.core.dropped_events.fetch_add(1, Ordering::AcqRel);
                self.core.overflow.store(true, Ordering::Release);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

impl Hub {
    /// Publish a committed announcement change (adapter calls this after the
    /// use case commits, never inside a storage transaction). Non-blocking;
    /// the same overflow policy as [`StreamingSink::publish`] applies.
    pub fn publish_announcement(&self, event: AnnouncementEvent) {
        match self.announcements.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.core.dropped_events.fetch_add(1, Ordering::AcqRel);
                self.core.overflow.store(true, Ordering::Release);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// An open connection's owner. Dropping it closes the connection and removes
/// its subscriptions, even when the client vanished mid-response.
pub struct Session {
    hub: Hub,
    conn: Arc<Connection>,
}

impl Session {
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }

    /// Subscribe this connection to `kind`. `Ok(false)` when the same channel
    /// set is already subscribed (upstream de-duplicates it).
    pub fn subscribe(&self, kind: StreamKind, names: Vec<String>) -> Result<bool, StreamError> {
        let notifications_scope = self.conn.scopes.iter().any(|scope| {
            scope == "read" || scope == "read:notifications"
        });
        let key = subscription_key(&channel_ids(&kind, self.conn.viewer, notifications_scope));
        let limits = &self.hub.core.limits;
        let mut registry = self.hub.core.registry();
        let subs = registry.subs.entry(self.conn.id).or_default();
        if subs.iter().any(|sub| sub.key == key) {
            return Ok(false);
        }
        if subs.len() >= limits.max_subscriptions_per_connection {
            return Err(StreamError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many streaming subscriptions",
            ));
        }
        subs.push(Subscription {
            conn_id: self.conn.id,
            viewer: self.conn.viewer,
            kind,
            names: names.into(),
            key,
            notifications_scope,
        });
        Ok(true)
    }

    /// Remove the subscription whose channel set matches `kind`.
    pub fn unsubscribe(&self, kind: &StreamKind) -> bool {
        let notifications_scope = self.conn.scopes.iter().any(|scope| {
            scope == "read" || scope == "read:notifications"
        });
        let key = subscription_key(&channel_ids(kind, self.conn.viewer, notifications_scope));
        let mut registry = self.hub.core.registry();
        let Some(subs) = registry.subs.get_mut(&self.conn.id) else {
            return false;
        };
        let before = subs.len();
        subs.retain(|sub| sub.key != key);
        before != subs.len()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.hub.close_connection(self.conn.id);
    }
}
