//! Bounded concurrency for expensive media work.
//!
//! Image decode/resize, blurhash and FFmpeg are CPU- and memory-heavy
//! blocking work. Running them on Tokio executor threads would stall every
//! other request, and spawning them without a bound would let a burst of
//! uploads exhaust the blocking pool and RAM. Two gates keep both in check:
//!
//! - [`MediaWorkGate`] queues async callers (no thread is held while waiting)
//!   and then runs the closure on the blocking pool, at most `limit` at a
//!   time;
//! - [`CountingGate`] is the synchronous counterpart used inside blocking
//!   code (FFmpeg process slots), where a thread may wait for a free slot.

use std::sync::{Arc, Condvar, Mutex, OnceLock};

use tokio::sync::Semaphore;

use crate::domain::{Error, Result};

/// Default number of concurrent image/media jobs.
pub const DEFAULT_MEDIA_WORK_LIMIT: usize = 4;

/// Default number of concurrent FFmpeg processes.
pub const DEFAULT_FFMPEG_LIMIT: usize = 2;

/// Runs blocking closures on the blocking pool, at most `limit` at once.
#[derive(Debug, Clone)]
pub struct MediaWorkGate {
    permits: Arc<Semaphore>,
}

impl MediaWorkGate {
    /// A gate admitting `limit` (at least one) concurrent jobs.
    pub fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit.max(1))),
        }
    }

    /// Permits currently free (observability/tests).
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// Wait for a slot, then run `work` off the executor.
    ///
    /// The slot is held until `work` returns even if the caller is dropped
    /// meanwhile (a disconnecting client cannot cancel work that already
    /// started, but it also cannot pile up more of it).
    pub async fn run<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| Error::Internal("media work gate closed".into()))?;
        tokio::task::spawn_blocking(move || {
            let output: T = work();
            drop(permit);
            output
        })
        .await
        .map_err(|err| Error::Internal(format!("media work task failed: {err}")))
    }
}

static MEDIA_WORK_GATE: OnceLock<MediaWorkGate> = OnceLock::new();

/// Fix the process-wide media work limit. Only the first call (before the
/// first [`media_work_gate`] use) takes effect.
pub fn configure_media_work_limit(limit: usize) {
    let _ = MEDIA_WORK_GATE.set(MediaWorkGate::new(limit));
}

/// The process-wide gate for upload processing.
pub fn media_work_gate() -> &'static MediaWorkGate {
    MEDIA_WORK_GATE.get_or_init(|| MediaWorkGate::new(DEFAULT_MEDIA_WORK_LIMIT))
}

/// A synchronous counting semaphore (blocks the calling thread for a slot).
#[derive(Debug)]
pub struct CountingGate {
    state: Mutex<GateState>,
    freed: Condvar,
}

#[derive(Debug)]
struct GateState {
    limit: usize,
    in_use: usize,
}

/// A held slot; released on drop.
#[derive(Debug)]
pub struct GatePermit<'gate> {
    gate: &'gate CountingGate,
}

impl CountingGate {
    /// A gate with `limit` (at least one) slots.
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                limit: limit.max(1),
                in_use: 0,
            }),
            freed: Condvar::new(),
        }
    }

    /// Change the slot count (takes effect for later acquisitions).
    pub fn set_limit(&self, limit: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.limit = limit.max(1);
        }
        self.freed.notify_all();
    }

    /// Slots currently held.
    pub fn in_use(&self) -> usize {
        self.state.lock().map(|state| state.in_use).unwrap_or(0)
    }

    /// Block until a slot is free and take it.
    pub fn acquire(&self) -> Result<GatePermit<'_>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("concurrency gate poisoned".into()))?;
        while state.in_use >= state.limit {
            state = self
                .freed
                .wait(state)
                .map_err(|_| Error::Internal("concurrency gate poisoned".into()))?;
        }
        state.in_use += 1;
        Ok(GatePermit { gate: self })
    }
}

impl Drop for GatePermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.in_use = state.in_use.saturating_sub(1);
        }
        self.gate.freed.notify_one();
    }
}

static FFMPEG_GATE: OnceLock<CountingGate> = OnceLock::new();

/// The process-wide gate for concurrent FFmpeg processes.
pub fn ffmpeg_gate() -> &'static CountingGate {
    FFMPEG_GATE.get_or_init(|| CountingGate::new(DEFAULT_FFMPEG_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn async_gate_never_exceeds_its_limit() {
        let gate = MediaWorkGate::new(2);
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let gate = gate.clone();
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                gate.run(move || {
                    let now: usize = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            }));
        }
        for task in tasks {
            task.await.expect("join").expect("run");
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "peak {}", peak.load(Ordering::SeqCst));
        assert_eq!(gate.available(), 2);
    }

    #[test]
    fn counting_gate_blocks_until_release() {
        let gate = Arc::new(CountingGate::new(1));
        let first = gate.acquire().expect("first");
        let worker = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let _second = gate.acquire().expect("second");
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(gate.in_use(), 1);
        drop(first);
        worker.join().expect("join");
        assert_eq!(gate.in_use(), 0);
    }
}
