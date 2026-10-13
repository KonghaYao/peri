//! flush/shutdown 屏障账本（契约 §3.6）：批次失败增量按水位记账，flush 确认独立于部署累计失败；
//! 这里不发任何请求，只结算 [super::BatchWorker] 提交结果。由 [`super`] 重导出。

use std::sync::Mutex;

use super::submission_result;
use crate::error::LangfuseError;

#[derive(Default)]
struct Watermarks {
    produced: u64,
    confirmed: u64,
}

#[derive(Default)]
pub(in crate::batcher) struct FailureLedger {
    watermarks: Mutex<Watermarks>,
}

/// A FIFO barrier's immutable view; it carries no event or HTTP response content.
#[derive(Clone, Copy)]
pub(in crate::batcher) struct FlushSnapshot {
    through: u64,
    failures: u64,
}

/// The worker's final lifetime count, unaffected by earlier flush confirmations.
#[derive(Clone, Copy)]
pub(in crate::batcher) struct ShutdownSnapshot {
    failures: u64,
}

impl ShutdownSnapshot {
    pub(in crate::batcher) fn result(self) -> Result<(), LangfuseError> {
        submission_result(self.failures)
    }
}

impl FailureLedger {
    pub(in crate::batcher) fn record_failure(&self) {
        let mut watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        watermarks.produced = watermarks
            .produced
            .checked_add(1)
            .expect("batch failure sequence exhausted");
    }

    pub(in crate::batcher) fn snapshot(&self) -> FlushSnapshot {
        let watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        FlushSnapshot {
            through: watermarks.produced,
            failures: watermarks.produced - watermarks.confirmed,
        }
    }

    pub(in crate::batcher) fn shutdown_snapshot(&self) -> ShutdownSnapshot {
        let watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        ShutdownSnapshot {
            failures: watermarks.produced,
        }
    }

    pub(in crate::batcher) fn observe(&self, snapshot: FlushSnapshot) -> Result<(), LangfuseError> {
        if snapshot.failures == 0 {
            return Ok(());
        }
        {
            let mut watermarks = self.watermarks.lock().expect("failure ledger poisoned");
            // Another caller may already have confirmed this or a newer barrier.
            // Never clear failures produced after this snapshot's watermark.
            watermarks.confirmed = watermarks.confirmed.max(snapshot.through);
        }
        submission_result(snapshot.failures)
    }
}
