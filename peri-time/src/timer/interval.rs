use super::{Duration, Future, Instant};
use std::fmt;
use std::fmt::{Debug, Formatter};

/// The supplied monotonic deadline expired before the future completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("deadline elapsed")]
pub struct Elapsed;

/// Wait for a duration. Dropping the returned future cancels its timer.
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Wait until a process-local monotonic deadline.
pub async fn sleep_until(deadline: Instant) {
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

/// Run a future until a relative budget expires. The future is dropped on expiry.
pub async fn timeout<F: Future>(budget: Duration, future: F) -> crate::Result<F::Output> {
    tokio::time::timeout(budget, future)
        .await
        .map_err(|_| Elapsed)
}

/// Run a future until an absolute process-local monotonic deadline.
pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> crate::Result<F::Output> {
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future)
        .await
        .map_err(|_| Elapsed)
}

/// A periodic process-local timer. Its first tick completes immediately.
pub struct Interval {
    pub(super) native: tokio::time::Interval,
}

impl Debug for Interval {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let mut interval = formatter.debug_struct("Interval");
        interval.field("native", &self.native);
        interval.finish_non_exhaustive()
    }
}

/// How an interval schedules its next tick after falling behind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissedTickBehavior {
    /// Emit overdue ticks promptly until the original schedule catches up.
    Burst,
    /// Start a fresh period after a missed tick.
    Delay,
    /// Preserve the original phase and skip missed ticks.
    Skip,
}

/// Create a periodic timer. A zero period panics, matching Tokio's contract.
#[must_use]
pub fn interval(period: Duration) -> Interval {
    assert!(!period.is_zero(), "interval period must be nonzero");
    Interval {
        native: tokio::time::interval(period),
    }
}

impl Interval {
    /// Choose how missed ticks affect the next scheduled deadline.
    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.native.set_missed_tick_behavior(match behavior {
            MissedTickBehavior::Burst => tokio::time::MissedTickBehavior::Burst,
            MissedTickBehavior::Delay => tokio::time::MissedTickBehavior::Delay,
            MissedTickBehavior::Skip => tokio::time::MissedTickBehavior::Skip,
        });
    }

    /// Wait for the next tick and return its scheduled monotonic instant.
    pub async fn tick(&mut self) -> Instant {
        self.native.tick().await.into_std()
    }
}
