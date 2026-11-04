//! Active expiration of terminal records without retaining the monitor instance.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::MonitorTask;
use crate::buffer_budget::BufferBudget;

type TaskRegistry = Mutex<HashMap<String, Arc<MonitorTask>>>;

pub(super) fn spawn(
    tasks: Weak<TaskRegistry>,
    buffers: Weak<BufferBudget>,
    cancel: CancellationToken,
    retention: Duration,
    sweep_interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(sweep_interval) => {}
            }
            let (Some(tasks), Some(buffers)) = (tasks.upgrade(), buffers.upgrade()) else {
                break;
            };
            let now = Instant::now();
            let mut tasks = tasks.lock();
            tasks.retain(|id, task| {
                let expired = task
                    .inner
                    .lock()
                    .finished_at_monotonic
                    .is_some_and(|finished| now.saturating_duration_since(finished) >= retention);
                if expired {
                    // Clear the actual ring before dropping registry ownership:
                    // an in-flight read may still hold an Arc to this task.
                    buffers.remove(id);
                }
                !expired
            });
        }
    });
}
