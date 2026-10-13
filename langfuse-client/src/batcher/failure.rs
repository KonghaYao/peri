//! 交付账本与终态记账（契约 §3.6、§5.2）：每个提交单元的终态（交付/重试/待修复/失败）在这里结算，
//! flush 确认独立于部署累计失败，账本之外不发任何请求。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::task::JoinError;
use tracing::{debug, error, warn};

use super::recovery::{Delivery, RetryEntry, Submission};
use super::worker::BatchWorker;
use crate::error::LangfuseError;
use crate::snapshot::TelemetryEvent;

use crate::DeliveryCounts;

mod barrier;

pub(in crate::batcher) use barrier::{FailureLedger, FlushSnapshot, ShutdownSnapshot};

/// 交付计数账本（契约 §3.6）：`pending` 是待交付全集，`persisted` 是其中的可恢复子集；字段分别读取，
/// 不是并发事务快照。`mark_persisted` 只在 payload 真的落入可恢复 Export 时登记，写盘本身从不是交付。
#[derive(Default)]
pub(super) struct DeliveryLedger {
    pending: AtomicU64,
    persisted: AtomicU64,
    /// 关闭移交的持久积压（不可撤回）；另一 owner 删除对象也不回退。
    handed_off: AtomicU64,
    failed: AtomicU64,
    cleanup: AtomicU64,
    active: AtomicU64,
    prepares: Mutex<PrepareFailures>,
}

/// 准备失败账本：按 `prepare_seq` 记账，屏障只覆盖自己前缀内的失败，累计值供关闭报告。
#[derive(Default)]
struct PrepareFailures {
    produced: u64,
    failed_seqs: BTreeSet<u64>,
}

impl DeliveryLedger {
    /// 准入成功：事件进入待交付集合。
    pub(super) fn accepted(&self, events: u64) {
        self.pending.fetch_add(events, Ordering::Relaxed);
    }

    /// 终态（送达/失败/本地拒收/驱逐）：离开待交付集合。
    pub(super) fn terminal(&self, events: u64) {
        saturating_release(&self.pending, events);
    }

    /// 已写入可恢复 Export、仍待交付：计入持久积压（pending 的子集）。
    pub(super) fn mark_persisted(&self, events: u64) {
        self.persisted.fetch_add(events, Ordering::Relaxed);
    }

    /// 持久积压事件到达终态：同时离开 pending 与 persisted。
    pub(super) fn deliver_persisted(&self, events: u64) {
        saturating_release(&self.pending, events);
        saturating_release(&self.persisted, events);
    }

    /// 无法确认整体交付的事件数（不重发整批）。
    pub(super) fn failed(&self, events: u64) {
        self.failed.fetch_add(events, Ordering::Relaxed);
    }

    pub(super) fn spawn_task(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn finish_task(&self) {
        saturating_release(&self.active, 1);
    }

    /// 待清理对象数由持有注册表的 worker 发布，`DeliveryCounts` 取同一份快照。
    pub(super) fn set_cleanup_pending(&self, objects: u64) {
        self.cleanup.store(objects, Ordering::Relaxed);
    }

    /// 关闭移交登记（契约 §5.2）：移交只发生一次，登记后本 owner 的终态不再升级为无积压。
    pub(super) fn record_handoff(&self, events: u64) {
        self.handed_off.fetch_add(events, Ordering::Relaxed);
    }

    pub(super) fn record_prepare_failure(&self, prepare_seq: u64) {
        let mut prepares = self.prepares.lock().unwrap_or_else(|p| p.into_inner());
        prepares.bump();
        prepares.failed_seqs.insert(prepare_seq);
    }

    /// 无法归属 `prepare_seq` 的准备失败（任务未正常结束）：只计生命周期账本，不入任何前缀。
    pub(super) fn record_unscoped_prepare_failure(&self) {
        let mut prepares = self.prepares.lock().unwrap_or_else(|p| p.into_inner());
        prepares.bump();
    }

    /// 确认并返回前缀 `prepare_seq <= boundary` 内尚未被观察的准备失败数；确认幂等、单调。
    pub(super) fn confirm_prepare_failures(&self, boundary: u64) -> u64 {
        let mut prepares = self.prepares.lock().unwrap_or_else(|p| p.into_inner());
        let mut confirmed = 0u64;
        while let Some(&seq) = prepares.failed_seqs.first() {
            if seq > boundary {
                break;
            }
            prepares.failed_seqs.remove(&seq);
            confirmed = confirmed
                .checked_add(1)
                .expect("prepare failure sequence exhausted");
        }
        confirmed
    }

    /// 生命周期内累计的准备失败数；`shutdown` 用它保证重复调用返回同一终态。
    pub(super) fn prepare_failures(&self) -> u64 {
        self.prepares
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .produced
    }

    pub(super) fn counts(&self) -> DeliveryCounts {
        let pending = self.pending.load(Ordering::Relaxed);
        // 持久积压是 pending 的子集；已移交事件取 `persisted` 与移交登记的并集，读侧夹取不出现 inverted 计数。
        let persisted = self.persisted.load(Ordering::Relaxed);
        let handed_off = self.handed_off.load(Ordering::Relaxed);
        let persisted = persisted.max(handed_off).min(pending);
        DeliveryCounts {
            pending_events: pending,
            persisted_pending_events: persisted,
            // 只计无法确认整体交付的事件；快照/准备失败是独立账本（`SnapshotPreparationFailed`
            // 与 `prepare_failures`），不污染事件语义（契约 §3.6）。
            failed_events: self.failed.load(Ordering::Relaxed),
            cleanup_pending_objects: self.cleanup.load(Ordering::Relaxed),
            active_tasks: self.active.load(Ordering::Relaxed),
        }
    }
}

impl PrepareFailures {
    fn bump(&mut self) {
        self.produced = self
            .produced
            .checked_add(1)
            .expect("prepare failures exhausted");
    }
}

/// 饱和递减：并发读计数不 panic、不溢出（观察值可能偏大，不据此宣告完成）。
fn saturating_release(counter: &AtomicU64, events: u64) {
    let release = |value: u64| Some(value.saturating_sub(events));
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, release);
}

fn submission_result(failures: u64) -> Result<(), LangfuseError> {
    if failures == 0 {
        return Ok(());
    }
    Err(LangfuseError::IngestionApi(format!(
        "{failures} batch submission(s) failed before the flush barrier"
    )))
}

impl BatchWorker {
    /// 在途提交单元的终态结算：只有 `Delivered` 离开待交付集合，其余四态都保留可见积压。
    pub(super) fn finish_send(&mut self, joined: Option<Result<Submission, JoinError>>) {
        self.ledger.finish_task();
        self.counters
            .completed_batches
            .fetch_add(1, Ordering::Relaxed);
        if self.in_flight.is_empty() {
            self.oldest_in_flight_ms = i64::MAX;
        }
        let Some(result) = joined else {
            return;
        };
        let Ok(submission) = result else {
            panic!("batch ingestion task did not complete normally");
        };
        let Submission {
            events,
            accepted_at_ms,
            expires_at_ms,
            persisted,
            prefix_seq,
            outcome,
        } = submission;
        match outcome {
            Delivery::Delivered => {
                if persisted {
                    self.ledger.deliver_persisted(events);
                } else {
                    self.ledger.terminal(events);
                }
                debug!(events, "telemetry batch delivered");
            }
            Delivery::Retry {
                payload,
                due_at_ms,
                error,
            } => {
                // 已落盘才能登记为持久积压：内存 payload 到期即出队，不冒充可恢复。
                let writable = !payload.is_memory();
                if writable && !persisted {
                    self.ledger.mark_persisted(events);
                }
                warn!(
                    events,
                    due_at_ms,
                    error = %error,
                    "telemetry batch deferred to its next allowed retry"
                );
                self.retry.push(RetryEntry {
                    payload,
                    accepted_at_ms,
                    expires_at_ms,
                    events,
                    persisted: writable,
                    handed_off: false,
                    due_at_ms: Some(due_at_ms),
                    prefix_seq,
                });
            }
            Delivery::Unresolved { payload, error } => {
                // 持久状态待修复：保留 payload 与 claim，停止发送，事件仍是 pending。
                warn!(
                    events,
                    error = %error,
                    "telemetry batch delivery unresolved; object retained for repair"
                );
                self.retry.push(RetryEntry {
                    payload,
                    accepted_at_ms,
                    expires_at_ms,
                    events,
                    persisted: false,
                    handed_off: false,
                    due_at_ms: None,
                    prefix_seq,
                });
            }
            Delivery::Failed { error, expired } => {
                if persisted {
                    self.ledger.deliver_persisted(events);
                } else {
                    self.ledger.terminal(events);
                }
                super::stats::note_spool_failure(&error, &self.counters);
                super::stats::note_partial_rejection(&error, &self.counters);
                if expired {
                    self.counters.add_expired_events(events);
                }
                self.ledger.failed(events);
                self.failures.record_failure();
                self.counters.failed_batches.fetch_add(1, Ordering::Relaxed);
                error!(events, expired, error = %error, "telemetry delivery terminated");
            }
        }
        self.publish_progress();
    }

    /// 超期终结：计数 + 保留在 `failed_events`，绝不当作送达；文件对象已由清理槽接管删除。
    pub(super) fn record_expired(&mut self, events: u64) {
        self.counters.add_expired_events(events);
        self.ledger.failed(events);
        self.failures.record_failure();
        self.counters.failed_batches.fetch_add(1, Ordering::Relaxed);
        error!(
            events,
            "telemetry spool object expired before delivery; delivery terminated"
        );
    }

    /// 批次在提交前失败（清理槽/冻结上下文不可用）：事件与来源一次性进入终态，不伪造成送达。
    pub(super) fn fail_batch(
        &mut self,
        events: Vec<TelemetryEvent>,
        count: u64,
        error: LangfuseError,
    ) {
        // 来源句柄在这里释放：Spooled 归原清理槽，由下一次有界 drain 删除。
        drop(events);
        super::stats::note_spool_failure(&error, &self.counters);
        self.failures.record_failure();
        self.counters.failed_batches.fetch_add(1, Ordering::Relaxed);
        self.ledger.failed(count);
        self.ledger.terminal(count);
        error!(events = count, error = %error, "Batcher: batch could not be submitted");
    }
}
