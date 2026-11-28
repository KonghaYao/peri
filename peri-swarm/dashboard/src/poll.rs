//! Pure polling policy: intervals, generations and exponential backoff.
//!
//! The UI owns the clock; this module only decides how long to wait and whether
//! a completed response is still relevant. Keeping it pure makes the timing
//! rules testable without a terminal or a gateway.

use std::time::Duration;

/// List interval while at most 100 sessions are loaded.
pub const LIST_INTERVAL_FAST: Duration = Duration::from_secs(3);
/// List interval while 101–300 sessions are loaded.
pub const LIST_INTERVAL_MEDIUM: Duration = Duration::from_secs(5);
/// List interval while more than 300 sessions are loaded.
pub const LIST_INTERVAL_SLOW: Duration = Duration::from_secs(10);
/// Detail interval while a turn is open or an approval is pending.
pub const DETAIL_INTERVAL_ACTIVE: Duration = Duration::from_secs(2);
/// Detail interval for a settled session.
pub const DETAIL_INTERVAL_IDLE: Duration = Duration::from_secs(5);
/// Health poll interval, independent of list polling.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(10);

const BACKOFF_SECONDS: [u64; 5] = [3, 6, 12, 24, 30];

/// List polling interval for the number of loaded sessions.
#[must_use]
pub fn list_interval(loaded: usize) -> Duration {
    match loaded {
        0..=100 => LIST_INTERVAL_FAST,
        101..=300 => LIST_INTERVAL_MEDIUM,
        _ => LIST_INTERVAL_SLOW,
    }
}

/// Detail polling interval for a session status.
#[must_use]
pub fn detail_interval(status: &str) -> Duration {
    match status {
        "in_progress" | "requires_action" => DETAIL_INTERVAL_ACTIVE,
        _ => DETAIL_INTERVAL_IDLE,
    }
}

/// Backoff after `failures` consecutive failures.
///
/// `None` means no backoff (no failures yet). The sequence is
/// 3 → 6 → 12 → 24 → 30 seconds, with 30 seconds as the cap.
#[must_use]
pub fn backoff_delay(failures: u32) -> Option<Duration> {
    if failures == 0 {
        return None;
    }
    let index = (failures as usize).min(BACKOFF_SECONDS.len()) - 1;
    Some(Duration::from_secs(BACKOFF_SECONDS[index]))
}

/// Combine a view's base interval with the current failure streak.
///
/// Backoff never makes a slow view poll faster than its successful interval.
#[must_use]
pub fn schedule_delay(base: Duration, failures: u32) -> Duration {
    backoff_delay(failures).map_or(base, |backoff| base.max(backoff))
}

/// Per-view polling state.
///
/// `generation` invalidates responses that belong to an older page/filter.
/// `in_flight` enforces one same-kind request at a time: even after an
/// invalidation, a new request waits until the old one completes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PollState {
    generation: u64,
    in_flight: bool,
    failures: u32,
    paused: bool,
}

impl PollState {
    /// A fresh, unpaused view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a request of this kind is currently running.
    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    /// Whether 401/overlay pause is active.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Consecutive failures since the last success.
    #[must_use]
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Invalidate outstanding responses (page change, filter change, refresh).
    ///
    /// An in-flight request keeps its slot so two same-kind requests never
    /// overlap; its response is discarded when it arrives.
    pub fn invalidate(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Start a request, returning the generation it belongs to.
    ///
    /// Returns `None` while paused or while another request is in flight.
    pub fn begin(&mut self) -> Option<u64> {
        if self.paused || self.in_flight {
            return None;
        }
        self.in_flight = true;
        Some(self.generation)
    }

    /// Record a response. Returns whether it is still current.
    ///
    /// A stale response clears the in-flight slot but neither resets nor
    /// increases the failure streak for the current generation.
    pub fn complete(&mut self, generation: u64) -> bool {
        self.in_flight = false;
        if generation != self.generation {
            return false;
        }
        self.failures = 0;
        true
    }

    /// Record a failure and return the delay to wait before retrying.
    ///
    /// A stale failure does not affect the current generation.
    pub fn fail(&mut self, generation: u64) -> Option<Duration> {
        self.in_flight = false;
        if generation != self.generation {
            return None;
        }
        self.failures = self.failures.saturating_add(1);
        backoff_delay(self.failures)
    }

    /// Pause polling (overlay/form open or 401) and discard outstanding data.
    pub fn pause(&mut self) {
        if !self.paused {
            self.paused = true;
            self.invalidate();
        }
    }

    /// Resume polling and discard data requested before the pause.
    pub fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            self.invalidate();
        }
    }

    /// Clear the failure streak after an explicit success (e.g. re-auth).
    pub fn reset_backoff(&mut self) {
        self.failures = 0;
    }

    /// Delay for this view, combining `base` with the failure streak.
    #[must_use]
    pub fn delay(&self, base: Duration) -> Duration {
        schedule_delay(base, self.failures)
    }
}

#[cfg(test)]
#[path = "poll_test.rs"]
mod tests;
