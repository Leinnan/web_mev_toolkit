//! Injectable wall clock, so tests can move time without sleeping or
//! back-dating rows.

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use time::OffsetDateTime;

/// Source of the current time.
pub trait Clock: Send + Sync + 'static {
    /// Current UTC time.
    fn now(&self) -> OffsetDateTime;

    /// Current time as unix seconds.
    fn unix(&self) -> i64 {
        self.now().unix_timestamp()
    }
}

/// The real system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A manually driven clock for tests. Clones share the same instant.
#[derive(Debug, Clone, Default)]
pub struct FixedClock(Arc<AtomicI64>);

impl FixedClock {
    /// Creates a clock frozen at `unix` seconds.
    #[must_use]
    pub fn at(unix: i64) -> Self {
        Self(Arc::new(AtomicI64::new(unix)))
    }

    /// Moves the clock to `unix` seconds.
    pub fn set(&self, unix: i64) {
        self.0.store(unix, Ordering::SeqCst);
    }

    /// Moves the clock forward by `seconds`.
    pub fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.unix()).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    fn unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Current Unix time in seconds, for code with no [`Clock`] at hand.
#[must_use]
pub fn unix_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::{Clock, FixedClock};

    #[test]
    fn fixed_clock_moves_only_when_told() {
        let clock = FixedClock::at(1_000);
        let shared = clock.clone();
        assert_eq!(clock.unix(), 1_000);
        shared.advance(86_400);
        assert_eq!(clock.unix(), 87_400);
        clock.set(5);
        assert_eq!(shared.now().unix_timestamp(), 5);
    }
}
