//! Peri's platform time boundary. Domain code owns timeout policy and persistent time fields.

mod calendar;
mod timer;

pub use calendar::{calendar_date, format_utc_rfc3339, CalendarConvention, CalendarDate};
pub use std::time::Instant;
pub use timer::{
    interval, sleep, sleep_until, timeout, timeout_at, Elapsed, Interval, MissedTickBehavior,
};

/// Result of a bounded wait.
pub type Result<T> = std::result::Result<T, Elapsed>;

use std::time::SystemTime;

/// Read the wall clock. This value can jump and must not measure elapsed time.
#[must_use]
pub fn now_wall() -> SystemTime {
    SystemTime::now()
}

/// Read a process-local monotonic clock.
#[must_use]
pub fn monotonic_now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// Measure elapsed process-local monotonic time since an earlier instant.
#[must_use]
pub fn elapsed_since(start: Instant) -> std::time::Duration {
    monotonic_now().saturating_duration_since(start)
}

/// Format a fresh wall-clock value as a UTC timestamp.
#[must_use]
pub fn now_utc_rfc3339() -> String {
    format_utc_rfc3339(now_wall())
}
