//! Mastodon-compatible numeric identifiers.
//!
//! Mastodon IDs (`Mastodon::Snowflake`) are 64-bit values composed of a
//! 48-bit millisecond timestamp shifted left by 16 bits, plus a 16-bit
//! sequence portion. REST responses serialize the numeric value as a decimal
//! string; ordering is numeric and must never be done lexicographically.
//!
//! We keep the value as a `u64` internally (numeric comparisons and range
//! scans) and only convert to/from the decimal string at the wire boundary.

use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

/// Number of bits reserved for the per-millisecond sequence portion.
const SEQUENCE_BITS: u32 = 16;
/// Mask covering the sequence portion.
const SEQUENCE_MASK: u64 = (1 << SEQUENCE_BITS) - 1;

/// A 64-bit timestamp-oriented identifier.
///
/// Serialized as a decimal string on the wire; stored as `u64` internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Id(u64);

/// Error parsing an [`Id`] from its decimal-string representation.
#[derive(Debug, thiserror::Error)]
#[error("invalid id: {0}")]
pub struct InvalidId(String);

impl Id {
    /// Wrap a raw 64-bit value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw numeric value. Used for storage, never for the wire format.
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Millisecond timestamp encoded in the high 48 bits.
    pub const fn timestamp_ms(self) -> u64 {
        self.0 >> SEQUENCE_BITS
    }

    /// Sequence portion from the low 16 bits.
    pub const fn sequence(self) -> u64 {
        self.0 & SEQUENCE_MASK
    }

    /// Build an ID for a given millisecond timestamp and sequence value,
    /// matching `Mastodon::Snowflake.id_at`.
    pub const fn from_parts(timestamp_ms: u64, sequence: u64) -> Self {
        Self((timestamp_ms << SEQUENCE_BITS) | (sequence & SEQUENCE_MASK))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Id {
    fn from(v: u64) -> Self {
        Self::new(v)
    }
}

impl From<Id> for u64 {
    fn from(id: Id) -> Self {
        id.0
    }
}

impl FromStr for Id {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Reject signs, whitespace, and other non-digit forms that `parse`
        // would otherwise accept or that would not round-trip.
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InvalidId(s.to_string()));
        }
        s.parse::<u64>()
            .map(Id::new)
            .map_err(|_| InvalidId(s.to_string()))
    }
}

impl serde::Serialize for Id {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Mastodon serializes numeric IDs as decimal strings.
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Id {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct IdVisitor;

        impl serde::de::Visitor<'_> for IdVisitor {
            type Value = u64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal id string or integer")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Id::from_str(v).map(Id::value).map_err(E::custom)
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(v)
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(IdVisitor).map(Id::new)
    }
}

/// Wall-clock time source for ID generation.
pub trait Clock: Send + Sync + 'static {
    /// Milliseconds since the Unix epoch.
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
    /// Generate the next strictly-increasing ID.
    fn generate(&self) -> Id;
}

/// Timestamp-oriented ID generator matching Mastodon's snowflake shape.
///
/// IDs are strictly increasing: within a millisecond the sequence portion is
/// incremented; when it would overflow (or the clock goes backwards) the
/// effective timestamp is advanced instead. This guarantees that numeric
/// ordering equals insertion order, which the cursor/pagination rules rely on.
pub struct TimestampIdGenerator {
    clock: Box<dyn Clock>,
    last: Mutex<u64>,
}

impl TimestampIdGenerator {
    /// Build a generator over the supplied clock.
    pub fn new(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            last: Mutex::new(0),
        }
    }
}

impl IdGenerator for TimestampIdGenerator {
    fn generate(&self) -> Id {
        let now = self.clock.now_ms();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());

        let candidate = if now > (*last >> SEQUENCE_BITS) {
            // New millisecond: start at sequence 0.
            now << SEQUENCE_BITS
        } else {
            // Same (or earlier) millisecond: continue the sequence. If the
            // sequence is exhausted, this naturally rolls into the next
            // timestamp, keeping IDs strictly increasing.
            *last + 1
        };

        *last = candidate;
        Id::new(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, manually advanced clock for tests.
    #[derive(Clone)]
    struct TestClock(std::sync::Arc<Mutex<u64>>);

    impl TestClock {
        fn new(start_ms: u64) -> Self {
            Self(std::sync::Arc::new(Mutex::new(start_ms)))
        }

        fn set(&self, ms: u64) {
            *self.0.lock().unwrap() = ms;
        }
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            *self.0.lock().unwrap()
        }
    }

    fn decode(id: Id) -> (u64, u64) {
        (id.timestamp_ms(), id.sequence())
    }

    #[test]
    fn id_serializes_as_decimal_string() {
        let id = Id::new(1234567890123456789);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"1234567890123456789\"");
    }

    #[test]
    fn id_deserializes_from_string_and_number() {
        let from_str: Id = serde_json::from_str("\"42\"").unwrap();
        let from_num: Id = serde_json::from_str("42").unwrap();
        assert_eq!(from_str, Id::new(42));
        assert_eq!(from_num, Id::new(42));
    }

    #[test]
    fn id_parse_rejects_non_digits() {
        assert!(" 1".parse::<Id>().is_err());
        assert!("-1".parse::<Id>().is_err());
        assert!("1.0".parse::<Id>().is_err());
        assert!("".parse::<Id>().is_err());
        assert_eq!("007".parse::<Id>().unwrap(), Id::new(7));
    }

    #[test]
    fn round_trip_parts() {
        let id = Id::from_parts(1_700_000_000_000, 7);
        assert_eq!(decode(id), (1_700_000_000_000, 7));
    }

    #[test]
    fn generator_is_strictly_increasing_within_millisecond() {
        let clock = TestClock::new(1_000);
        let generator = TimestampIdGenerator::new(Box::new(clock.clone()));
        let mut prev = generator.generate();
        for _ in 0..10_000 {
            let next = generator.generate();
            assert!(next > prev, "{next} !> {prev}");
            prev = next;
        }
        // All IDs share the same millisecond and the sequence advanced.
        assert_eq!(prev.timestamp_ms(), 1_000);
        assert_eq!(prev.sequence(), 10_000);
    }

    #[test]
    fn generator_advances_with_clock_and_never_goes_backwards() {
        let clock = TestClock::new(1_000);
        let generator = TimestampIdGenerator::new(Box::new(clock.clone()));
        let a = generator.generate();
        clock.set(2_000);
        let b = generator.generate();
        assert!(b > a);
        assert_eq!(b.timestamp_ms(), 2_000);

        // A backwards clock must not produce a smaller ID.
        clock.set(1_500);
        let c = generator.generate();
        assert!(c > b);
    }

    #[test]
    fn numeric_order_matches_time_order_not_lexicographic() {
        // Build two IDs a second apart; the decimal strings would still order
        // correctly here, but the numeric comparison must be the authority.
        let older = Id::from_parts(1_000, 0);
        let newer = Id::from_parts(1_001, 0);
        assert!(older < newer);

        let ids: Vec<&str> = vec!["1000", "999"];
        // Lexicographic would put "1000" < "999"; numeric does not.
        let lex = {
            let mut v = ids.clone();
            v.sort();
            v
        };
        assert_eq!(lex, vec!["1000", "999"]);
        let numeric = {
            let mut v: Vec<Id> = ids.iter().map(|s| s.parse().unwrap()).collect();
            v.sort();
            v.iter().map(|i| i.to_string()).collect::<Vec<_>>()
        };
        assert_eq!(numeric, vec!["999", "1000"]);
    }
}
