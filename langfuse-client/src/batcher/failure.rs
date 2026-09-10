//! Flush errors are confirmed by the caller observing a snapshot, never by ack send.

use std::sync::Mutex;

use crate::LangfuseError;

#[derive(Default)]
struct Watermarks {
    produced: u64,
    confirmed: u64,
}

#[derive(Default)]
pub(super) struct FailureLedger {
    watermarks: Mutex<Watermarks>,
}

/// A FIFO barrier's immutable view; it carries no event or HTTP response content.
pub(super) struct FlushSnapshot {
    through: u64,
    failures: u64,
}

impl FailureLedger {
    pub(super) fn record_failure(&self) {
        let mut watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        watermarks.produced = watermarks
            .produced
            .checked_add(1)
            .expect("batch failure sequence exhausted");
    }

    pub(super) fn snapshot(&self) -> FlushSnapshot {
        let watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        FlushSnapshot {
            through: watermarks.produced,
            failures: watermarks.produced - watermarks.confirmed,
        }
    }

    pub(super) fn observe(&self, snapshot: FlushSnapshot) -> Result<(), LangfuseError> {
        if snapshot.failures == 0 {
            return Ok(());
        }
        {
            let mut watermarks = self.watermarks.lock().expect("failure ledger poisoned");
            // Another caller may already have confirmed this or a newer barrier.
            // Never clear failures produced after this snapshot's watermark.
            watermarks.confirmed = watermarks.confirmed.max(snapshot.through);
        }
        Err(LangfuseError::IngestionApi(format!(
            "{} batch submission(s) failed before the flush barrier",
            snapshot.failures
        )))
    }
}
