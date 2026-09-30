use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatcherStats {
    pub accepted_events: usize,
    pub rejected_events: usize,
    pub evicted_events: usize,
    pub submitted_batches: usize,
    pub completed_batches: usize,
    pub failed_batches: usize,
    pub partially_rejected_batches: usize,
    pub rejected_spans: u64,
    pub in_flight_batches: usize,
}

#[derive(Default)]
pub(super) struct Counters {
    pub accepted_events: AtomicUsize,
    pub rejected_events: AtomicUsize,
    pub evicted_events: AtomicUsize,
    pub submitted_batches: AtomicUsize,
    pub completed_batches: AtomicUsize,
    pub failed_batches: AtomicUsize,
    pub partially_rejected_batches: AtomicUsize,
    pub rejected_spans: AtomicU64,
    pub in_flight_batches: AtomicUsize,
}

impl Counters {
    pub(super) fn snapshot(&self) -> BatcherStats {
        BatcherStats {
            accepted_events: self.accepted_events.load(Ordering::Relaxed),
            rejected_events: self.rejected_events.load(Ordering::Relaxed),
            evicted_events: self.evicted_events.load(Ordering::Relaxed),
            submitted_batches: self.submitted_batches.load(Ordering::Relaxed),
            completed_batches: self.completed_batches.load(Ordering::Relaxed),
            failed_batches: self.failed_batches.load(Ordering::Relaxed),
            partially_rejected_batches: self.partially_rejected_batches.load(Ordering::Relaxed),
            rejected_spans: self.rejected_spans.load(Ordering::Relaxed),
            in_flight_batches: self.in_flight_batches.load(Ordering::Relaxed),
        }
    }
}
