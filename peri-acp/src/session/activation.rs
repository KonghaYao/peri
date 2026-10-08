use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use peri_acp_types::session::MessageQueue;

#[derive(Default)]
pub(crate) struct SessionActivation {
    pub(crate) listener_started: AtomicBool,
    allowed: AtomicBool,
    failed_input_watermark: AtomicU64,
}

impl SessionActivation {
    pub(crate) fn allow(&self) {
        self.failed_input_watermark.store(0, Ordering::Release);
        self.allowed.store(true, Ordering::Release);
    }

    pub(crate) fn suppress(&self) {
        self.allowed.store(false, Ordering::Release);
    }

    pub(crate) fn is_allowed(&self) -> bool {
        self.allowed.load(Ordering::Acquire)
    }

    pub(crate) fn record_failed_attempt(&self, watermark: u64) {
        self.failed_input_watermark
            .fetch_max(watermark, Ordering::AcqRel);
    }

    pub(crate) fn can_activate(&self, queue: &MessageQueue) -> bool {
        self.is_allowed()
            && queue
                .has_ensure_processing_after(self.failed_input_watermark.load(Ordering::Acquire))
    }
}

#[cfg(test)]
#[path = "activation_test.rs"]
mod tests;
