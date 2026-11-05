use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;
use peri_acp_types::session::{MessageQueue, MqWatermark};
use peri_agent::session::async_result_mailbox::AsyncResultMailbox;

#[derive(Default)]
pub(crate) struct SessionActivation {
    pub(crate) listener_started: AtomicBool,
    allowed: AtomicBool,
    /// 进程内显式 Stop 意图：`suppress()` 置位、`allow()` 清除。
    ///
    /// 与 `allowed` 分开：`allowed` 还会被失败尝试复位，恢复入口只信用户是否显式停止。
    user_stop: AtomicBool,
    /// 最近一次失败尝试开始时 MQ 接纳水位；只检查其后的 EnsureProcessing。
    failed_input_watermark: Mutex<Option<MqWatermark>>,
    /// 最近一次失败尝试开始时 mailbox 已接纳结果的最大代数。只有其后新到达的
    /// Ready 结果才能触发自动尝试；失败记录本身不能自我激活。
    failed_result_watermark: AtomicU64,
}

impl SessionActivation {
    pub(crate) fn allow(&self) {
        *self.failed_input_watermark.lock() = None;
        self.failed_result_watermark.store(0, Ordering::Release);
        self.user_stop.store(false, Ordering::Release);
        self.allowed.store(true, Ordering::Release);
    }

    pub(crate) fn suppress(&self) {
        self.user_stop.store(true, Ordering::Release);
        self.allowed.store(false, Ordering::Release);
    }

    pub(crate) fn is_allowed(&self) -> bool {
        self.allowed.load(Ordering::Acquire)
    }

    /// 本进程内用户是否显式停止过（恢复入口据此不重新放行执行）。
    pub(crate) fn user_stopped(&self) -> bool {
        self.user_stop.load(Ordering::Acquire)
    }

    pub(crate) fn record_failed_attempt(&self, watermark: Option<MqWatermark>) {
        let mut failed = self.failed_input_watermark.lock();
        // 水位只前进：并发失败尝试不能把已观察到的更晚事实退回。
        if failed
            .as_ref()
            .is_none_or(|current| watermark.as_ref() > Some(current))
        {
            *failed = watermark;
        }
    }

    pub(crate) fn record_failed_result_watermark(&self, watermark: u64) {
        self.failed_result_watermark
            .fetch_max(watermark, Ordering::AcqRel);
    }

    pub(crate) async fn can_activate(&self, queue: &MessageQueue) -> bool {
        if !self.is_allowed() {
            return false;
        }
        let watermark = self.failed_input_watermark.lock().clone();
        self.is_allowed()
            && queue
                .has_ensure_processing_after(&watermark)
                .await
                .unwrap_or(false)
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
