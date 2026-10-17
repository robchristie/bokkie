//! Wall-clock time, replaceable in tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use bokkie_protocol::Timestamp;

/// The gateway's clock, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    manual: Option<Arc<AtomicI64>>,
}

impl Clock {
    pub fn system() -> Self {
        Self::default()
    }

    /// A clock that stands still until [`Clock::advance`] moves it.
    pub fn manual(start_ms: i64) -> Self {
        Self {
            manual: Some(Arc::new(AtomicI64::new(start_ms))),
        }
    }

    pub fn now_ms(&self) -> i64 {
        match &self.manual {
            Some(now) => now.load(Ordering::SeqCst),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default(),
        }
    }

    pub fn now(&self) -> Timestamp {
        Timestamp(self.now_ms())
    }

    /// Moves a manual clock forward. Does nothing to the system clock.
    pub fn advance(&self, ms: i64) {
        if let Some(now) = &self.manual {
            now.fetch_add(ms, Ordering::SeqCst);
        }
    }
}

pub const MINUTE_MS: i64 = 60 * 1000;
pub const HOUR_MS: i64 = 60 * MINUTE_MS;
pub const DAY_MS: i64 = 24 * HOUR_MS;
