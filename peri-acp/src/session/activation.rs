use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use peri_acp_types::session::MessageQueue;
use peri_agent::session::async_result_mailbox::AsyncResultMailbox;

#[derive(Default)]
pub(crate) struct SessionActivation {
    pub(crate) listener_started: AtomicBool,
    allowed: AtomicBool,
    failed_input_watermark: AtomicU64,
    /// 最近一次失败尝试开始时 mailbox 已接纳结果的最大代数。只有其后新到达的
    /// Ready 结果才能触发自动尝试；失败记录本身不能自我激活。
    failed_result_watermark: AtomicU64,
}

impl SessionActivation {
    pub(crate) fn allow(&self) {
        self.failed_input_watermark.store(0, Ordering::Release);
        self.failed_result_watermark.store(0, Ordering::Release);
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

    pub(crate) fn record_failed_result_watermark(&self, watermark: u64) {
        self.failed_result_watermark
            .fetch_max(watermark, Ordering::AcqRel);
    }

    pub(crate) fn can_activate(&self, queue: &MessageQueue) -> bool {
        self.is_allowed()
            && queue
                .has_ensure_processing_after(self.failed_input_watermark.load(Ordering::Acquire))
    }

    /// Ready 结果是否构成新的、允许自动尝试的工作来源。
    ///
    /// Stop 抑制优先（`allowed == false`），失败水位之后接纳的结果才算新工作。
    pub(crate) fn can_activate_result(&self, mailbox: &AsyncResultMailbox) -> bool {
        self.is_allowed()
            && mailbox.has_ready_after(self.failed_result_watermark.load(Ordering::Acquire))
    }
}

#[cfg(test)]
#[path = "activation_test.rs"]
mod tests;
