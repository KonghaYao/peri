//! Worker 生命周期的收敛：唯一 owner 的句柄保留、准备任务派发与收敛、关闭排空、清理收敛与移交。

use std::sync::{atomic::Ordering, Arc};

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use super::{
    admission::{CommandReceiver, PrepareRequest},
    failure::{DeliveryLedger, ShutdownSnapshot},
    freeze::{freeze_snapshot, InputSpool},
    recovery::{self, HandOff, RetryEntry, RetryQueue, Submission},
    stats::Counters,
    worker::BatchWorker,
    FlushBoundary,
};
use crate::client::LangfuseClient;
use crate::config::TelemetryConfig;
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::snapshot::SnapshotState;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::SpoolStore;

pub(super) enum WorkerOwner {
    Running(JoinHandle<WorkerExit>),
    Joined(Result<WorkerExit, WorkerFailure>),
}

/// worker 终态 + 关闭后仍归本 owner 的收敛状态。
pub(super) struct WorkerExit {
    snapshot: ShutdownSnapshot,
    pub(super) retained: RetainedOwner,
}

impl WorkerExit {
    /// worker 正常返回：交付失败账本 + 保留给后续收敛的全部 owner 状态。
    pub(super) fn new(
        snapshot: ShutdownSnapshot,
        registry: Option<CleanupRegistry>,
        retry: RetryQueue,
        spool: Option<Arc<dyn SpoolStore>>,
        ledger: Arc<DeliveryLedger>,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            snapshot,
            retained: RetainedOwner {
                registry,
                retry,
                spool,
                ledger,
                counters,
            },
        }
    }
}

/// 关闭后仍归本 owner 的状态：清理槽、未收敛 payload 与发布计数所需的账本。
///
/// 关闭期限到期、删除失败或持久状态待修复时，这些对象既不能删也不能丢；下一次 `shutdown`
/// 用同一个 owner 重入收敛，从而跨多次宿主关闭真正收敛（契约 §5.2、§3.2）。
pub(super) struct RetainedOwner {
    registry: Option<CleanupRegistry>,
    retry: RetryQueue,
    spool: Option<Arc<dyn SpoolStore>>,
    ledger: Arc<DeliveryLedger>,
    counters: Arc<Counters>,
}

impl RetainedOwner {
    /// 再入收敛：重试有界清理（失败保留槽与配额），推进仍可移交的持久条目（先记账再解锁）。
    /// 返回 `true` 表示仍有本地未决：待清理槽或保留的 payload。
    pub(super) async fn converge(&mut self) -> bool {
        drain_cleanup(
            self.registry.as_mut(),
            &self.ledger,
            self.spool.as_ref(),
            &self.counters,
        )
        .await;
        let plan = self.retry.plan_hand_off();
        self.ledger.record_handoff(plan.hand_off_events());
        let handed = self.retry.release_hand_off(plan, self.registry.as_ref());
        if handed.local_pending_events > 0 {
            warn!(
                events = handed.local_pending_events,
                "telemetry events remain local after shutdown; delivery stays pending"
            );
        }
        let cleanup_pending = self
            .registry
            .as_ref()
            .is_some_and(|registry| registry.pending().0 > 0);
        cleanup_pending || !self.retry.is_empty()
    }
}

#[derive(Clone, Copy)]
pub(super) struct WorkerFailure {
    cancelled: bool,
}

impl WorkerOwner {
    pub(super) async fn join(&mut self) -> Result<(), LangfuseError> {
        if let Self::Running(handle) = self {
            // Borrow, never take: cancelling this future leaves the handle in
            // the owner for another caller to join. No await after completion
            // until the immutable terminal outcome is installed.
            let outcome = handle.await.map_err(|error| WorkerFailure {
                cancelled: error.is_cancelled(),
            });
            *self = Self::Joined(outcome);
        }
        match self {
            Self::Joined(Ok(exit)) => exit.snapshot.result(),
            Self::Joined(Err(failure)) => Err(LangfuseError::WorkerJoinFailed {
                cancelled: failure.cancelled,
            }),
            Self::Running(_) => unreachable!("worker join must publish a terminal outcome"),
        }
    }

    /// 已 join 终态的保留状态；未 join 或 join 失败时是 `None`。
    pub(super) fn retained(&mut self) -> Option<&mut RetainedOwner> {
        match self {
            Self::Joined(Ok(exit)) => Some(&mut exit.retained),
            _ => None,
        }
    }
}

/// 批次任务存活计数：任务被 abort 也必须归还 `in_flight_batches`。
pub(super) struct SendCounter(pub(super) Arc<Counters>);

impl Drop for SendCounter {
    fn drop(&mut self) {
        self.0.in_flight_batches.fetch_sub(1, Ordering::Relaxed);
    }
}

impl BatchWorker {
    /// 派发并 join 全部已登记的准备任务：屏障与关闭都靠它把来源收敛到终态。
    pub(super) async fn drain_prepares(&mut self, rx: &CommandReceiver) {
        self.drain_prepares_upto(rx, u64::MAX).await
    }

    /// 只收敛 `prepare_seq <= boundary` 的登记：后缀准备不进入本次前缀屏障。
    pub(super) async fn drain_prepares_upto(&mut self, rx: &CommandReceiver, boundary: u64) {
        loop {
            self.dispatch_prepares_upto(rx, boundary);
            let Some(joined) = self.prepares.join_next().await else {
                return;
            };
            self.finish_prepare(Some(joined));
        }
    }

    /// 取走并提交 FIFO 准备任务；并发数受 `prepare_max_in_flight` 约束。
    pub(super) fn dispatch_prepares(&mut self, rx: &CommandReceiver) {
        self.dispatch_prepares_upto(rx, u64::MAX)
    }

    /// 同 [`Self::dispatch_prepares`]，但只提交水位之内的登记（屏障按水位推进）。
    pub(super) fn dispatch_prepares_upto(&mut self, rx: &CommandReceiver, boundary: u64) {
        while self.prepares.len() < self.prepare_max_in_flight {
            let Some(request) = rx.take_prepare_upto(boundary) else {
                return;
            };
            self.spawn_prepare(request);
        }
    }

    fn spawn_prepare(&mut self, request: PrepareRequest) {
        self.ledger.spawn_task();
        let prepare_seq = request.cell.prepare_seq;
        let budget = Arc::clone(&self.budget);
        let ledger = Arc::clone(&self.ledger);
        let client = Arc::clone(&self.client);
        let telemetry = self.telemetry.clone();
        let max_event_bytes = self.limits.max_event_bytes;
        // 准备中的来源 retained + 输出 allowance 记为进度账，任务结束时归还。
        let reserved = request
            .retained
            .bytes()
            .saturating_add(request.allowance.bytes());
        self.counters.add_preparing_bytes(reserved as u64);
        // 清理槽先于任何 create 分配（同步、无 IO）；容量不足即按准备失败登记，不留下半登记。
        let slot = match (&self.spool, &self.registry) {
            (Some(store), Some(registry)) if self.telemetry.spool_enabled => registry
                .try_register()
                .map(|registration| Some((Arc::clone(store), registration)))
                .map_err(LangfuseError::Spool),
            _ => Ok(None),
        };
        self.prepares.spawn(async move {
            let PrepareRequest {
                cell,
                source,
                allowance,
                retained,
                ..
            } = request;
            let cancelled = cell.cancelled.load(Ordering::Acquire);
            // 准备阶段工作区租约：等待并发槽可以调度，但不等待 retained 额度。
            let outcome = if cancelled {
                Err(LangfuseError::ChannelClosed)
            } else {
                match budget.try_prepare_workspace() {
                    Ok(_lease) => slot.and_then(|slot| {
                        input_spool(slot, &client, &telemetry, max_event_bytes, &budget)
                    }),
                    Err(error) => Err(error),
                }
                .and_then(|spool| freeze_snapshot(&source, allowance, spool))
            };
            // 源 retained 额度在内容转成快照字节/失败后立即归还。
            drop(retained);
            if outcome.is_err() {
                ledger.record_prepare_failure(prepare_seq);
            }
            let mut state = cell.state.lock().unwrap_or_else(|p| p.into_inner());
            *state = match outcome {
                Ok(snapshot) => SnapshotState::Ready(snapshot),
                Err(error) => SnapshotState::Failed(error),
            };
            drop(state);
            cell.changed.notify_waiters();
            (prepare_seq, reserved)
        });
    }

    pub(super) fn finish_prepare(
        &mut self,
        joined: Option<Result<(u64, usize), tokio::task::JoinError>>,
    ) {
        self.ledger.finish_task();
        match joined {
            Some(Ok((_seq, reserved))) => {
                self.counters.release_preparing_bytes(reserved as u64);
            }
            // 取消/panic 的准备任务不再归还进度账：这里只登记未归属失败，计数保守偏大。
            Some(Err(join_error)) if !join_error.is_cancelled() => {
                self.ledger.record_unscoped_prepare_failure();
                error!("Batcher snapshot preparation task did not complete normally");
            }
            Some(Err(_)) | None => {}
        }
    }

    /// 准入关闭后的排空：先收尽已提交命令，再收敛准备/重试/在途，最后有界清理并移交持久积压。
    ///
    /// 返回移交分类；调用方负责登记与汇总日志，本函数不删除任何对象。
    pub(super) async fn drain_after_close(&mut self, rx: &mut CommandReceiver) -> HandOff {
        // Admission is closed before the signal. No producer can commit after
        // this point. Drain committed commands only: recv().await could wait on
        // a reserved permit belonging to a suspended, unpolled producer.
        while let Some(command) = rx.try_recv() {
            self.process(command, rx).await;
        }
        let remaining = self.queued_len();
        if remaining > 0 {
            info!("Batcher shutting down, flushing {remaining} remaining events");
        }
        // 关闭排空：先收敛全部准备登记（取消也显式记账），再按 FIFO 前缀提交；已到期的重试条目
        // 仍可发送，未到期的由移交处理。无在途批次又提交不了才停下（由 `Pending` 暴露未排空）。
        loop {
            self.drain_prepares(rx).await;
            self.dispatch_retries().await;
            self.pump_batches(None).await;
            let Some(joined) = self.in_flight.join_next().await else {
                break;
            };
            self.finish_send(Some(joined));
            if self.queued_len() == 0 && !self.retry.has_actionable(super::wall_ms()) {
                break;
            }
        }
        self.drain_sends().await;
        // 最后一次有界清理：此后不再删除或移交任何对象。
        drain_cleanup(
            self.registry.as_mut(),
            &self.ledger,
            self.spool.as_ref(),
            &self.counters,
        )
        .await;
        super::stats::report_dropped(&self.dropped);
        // 唯一正常移交点：**先**登记不可撤回的持久积压，**再**释放 claim 与本地清理槽，
        // 另一进程可领取。未收敛的条目留在队列里随终态交回 owner。
        let plan = self.retry.plan_hand_off();
        self.ledger.record_handoff(plan.hand_off_events());
        self.retry.release_hand_off(plan, self.registry.as_ref())
    }
    /// FIFO 前缀屏障：提交并等待 `event_seq <= E` 与 `prepare_seq <= P` 的全部工作。
    pub(super) async fn converge(&mut self, rx: &CommandReceiver, boundary: FlushBoundary) {
        loop {
            // 屏障只推进自己的双水位：后缀准备不派发、不入队，也不占满 worker 让前缀饥饿。
            self.dispatch_prepares_upto(rx, boundary.prepare_seq);
            self.pump_batches(Some(boundary.event_seq)).await;
            while !self.in_flight.is_empty() {
                let joined = self.in_flight.join_next().await;
                self.finish_send(joined);
            }
            self.drain_prepares_upto(rx, boundary.prepare_seq).await;
            let prepares_left = rx.pending_prepare_upto(boundary.prepare_seq);
            // 已解析前缀先于未解析尾部，两者都按 FIFO：任一头事件仍在边界内即未收敛。
            let planned = self.planned.front().map(|(queued, _)| queued);
            let front = planned.or(self.buffer.front());
            let prefix_left = front.is_some_and(|queued| queued.seq <= boundary.event_seq);
            if !prepares_left && !prefix_left {
                self.publish_progress();
                return;
            }
            // 前缀未终态只可能是等待某个准备 cell 收敛：其登记要么已取走（上面已 join），要么还在
            // 队列里（上面已 dispatch）。让出执行权后重试，不轮询、不虚增 pending。
            tokio::task::yield_now().await;
        }
    }

    /// 到期重试：TTL 先到的条目终结并清理，其余按同一发送单元续发（与 live 批次共用 JoinSet）。
    pub(super) async fn dispatch_retries(&mut self) {
        let now_ms = super::wall_ms();
        while self.in_flight.len() < self.max_in_flight {
            let Some(entry) = self.retry.take_actionable(now_ms) else {
                return;
            };
            if entry.expires_at_ms <= now_ms {
                let events = entry.events;
                match recovery::expire_entry(entry).await {
                    Ok(()) => self.record_expired(events),
                    Err((mut entry, error)) => {
                        // 持久终态写不进去：保留对象与 claim，标记待修复、停止自动重试。
                        entry.due_at_ms = None;
                        warn!(
                            events = entry.events,
                            error = %error,
                            "telemetry spool expiry could not be recorded; delivery stays pending"
                        );
                        self.retry.push(entry);
                        return;
                    }
                }
                continue;
            }
            self.spawn_retry(entry);
        }
    }

    fn spawn_retry(&mut self, entry: RetryEntry) {
        let RetryEntry {
            payload,
            accepted_at_ms,
            expires_at_ms,
            events,
            persisted,
            prefix_seq,
            ..
        } = entry;
        let client = Arc::clone(&self.client);
        self.counters.add_retry_attempt();
        self.ledger.spawn_task();
        self.counters
            .in_flight_batches
            .fetch_add(1, Ordering::Relaxed);
        let counter = SendCounter(Arc::clone(&self.counters));
        self.track_in_flight(accepted_at_ms);
        self.in_flight.spawn(async move {
            let _counter = counter;
            let export = client.export_config().clone();
            let outcome = recovery::send_export(&client, payload, expires_at_ms, &export).await;
            Submission {
                events,
                accepted_at_ms,
                expires_at_ms,
                persisted,
                // 续发不改变前缀归属：重新失败后仍须让覆盖它的屏障看到未终态。
                prefix_seq,
                outcome,
            }
        });
    }
}

/// 有界 blocking 清理：每轮至多处理当下可见的待清理槽；失败保留槽与配额由下次重试。
pub(super) async fn drain_cleanup(
    registry: Option<&mut CleanupRegistry>,
    ledger: &DeliveryLedger,
    store: Option<&Arc<dyn SpoolStore>>,
    counters: &Counters,
) {
    let Some(registry) = registry else {
        return;
    };
    for _ in 0..8 {
        if registry.pending().0 == 0 {
            break;
        }
        if let Err(error) = registry.drain().await {
            warn!(error = %error, "telemetry spool cleanup deferred to the next round");
            break;
        }
    }
    ledger.set_cleanup_pending(registry.pending().0 as u64);
    super::stats::refresh_spool_usage(store, counters).await;
}

/// 冻结落盘上下文：`None` 表示该部署只走有界内存路径（未开启暂存或端口不可用）。
fn input_spool(
    slot: Option<(Arc<dyn SpoolStore>, crate::spool::CleanupRegistration)>,
    client: &LangfuseClient,
    telemetry: &TelemetryConfig,
    max_bytes: usize,
    budget: &Arc<MemoryBudget>,
) -> Result<Option<(InputSpool, crate::spool::CleanupRegistration)>, LangfuseError> {
    let Some((store, cleanup)) = slot else {
        return Ok(None);
    };
    let context = client.export_context(telemetry.spool_ttl)?;
    Ok(Some((
        InputSpool {
            store,
            budget: Arc::clone(budget),
            threshold: telemetry.input_spool_threshold_bytes,
            max_bytes,
            route: context.route,
            accepted_at_ms: context.accepted_at_ms,
            expires_at_ms: context.expires_at_ms,
        },
        cleanup,
    )))
}
