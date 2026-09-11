//! Flush confirmation advances independently of the deployment's cumulative failures.

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
#[derive(Clone, Copy)]
pub(super) struct FlushSnapshot {
    through: u64,
    failures: u64,
}

/// The worker's final lifetime count, unaffected by earlier flush confirmations.
#[derive(Clone, Copy)]
pub(super) struct ShutdownSnapshot {
    failures: u64,
}

impl ShutdownSnapshot {
    pub(super) fn result(self) -> Result<(), LangfuseError> {
        submission_result(self.failures)
    }
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

    pub(super) fn shutdown_snapshot(&self) -> ShutdownSnapshot {
        let watermarks = self.watermarks.lock().expect("failure ledger poisoned");
        ShutdownSnapshot {
            failures: watermarks.produced,
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
        submission_result(snapshot.failures)
    }
}

fn submission_result(failures: u64) -> Result<(), LangfuseError> {
    if failures == 0 {
        Ok(())
    } else {
        Err(LangfuseError::IngestionApi(format!(
            "{failures} batch submission(s) failed before the flush barrier"
        )))
    }
}
