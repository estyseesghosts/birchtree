use std::sync::atomic::{AtomicU64, Ordering};

/// 64-bit timestamp-oriented identifier, Mastodon-shaped.
///
/// Serialized on the wire as a decimal string. Comparisons are numeric; never
/// order decimal IDs lexicographically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(u64);

impl Id {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Id {
    fn from(v: u64) -> Self {
        Self::new(v)
    }
}

/// Wall-clock time source for ID generation.
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_millis() as u64
    }
}

/// System wall clock.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock;

/// ID generator trait.
pub trait IdGenerator: Send + Sync + 'static {
    fn generate(&self) -> Id;
}

/// Timestamp-oriented ID generator.
///
/// Mastodon's snowflake carries a millisecond timestamp component plus
/// sequence data. This is a minimal Phase 0/1 shape; refine against
/// `lib/mastodon/snowflake.rb` in Phase 1.
pub struct TimestampIdGenerator {
    clock: Box<dyn Clock>,
    sequence: AtomicU64,
}

impl TimestampIdGenerator {
    pub fn new(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            sequence: AtomicU64::new(0),
        }
    }
}

impl IdGenerator for TimestampIdGenerator {
    fn generate(&self) -> Id {
        let ts = self.clock.now_ms();
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed) & 0x3FFF;
        Id::new((ts << 14) | seq)
    }
}
