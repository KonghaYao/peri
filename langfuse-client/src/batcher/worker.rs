//! 唯一 worker：有界准备队列、FIFO 前缀屏障、启动恢复与重试续发，以及全部有界发送任务。
//!
//! 批次一律经流式编码产生 [`ExportPayload`]，再交 [`recovery::send_export`]：冻结 input/output 的
//! 正文随 payload 上线，恢复对象与 live 批次走同一条路径；只有 `Delivered` 是交付，只写盘未送达的
//! 事件留在 `pending`/`persisted` 账本里等待续发或移交。`Retryable` 持久写回 `retry_wait` 后由同一
//! worker 到期续发（首次时间/TTL 不变），一次调度一个唤醒。
//!
//! 跨文件 `impl BatchWorker`：准备任务收敛与关闭排空在 [`super::shutdown`]，交付终态记账在
//! [`super::failure`]；本模块字段对 `batcher` 内可见，方法按需要 `pub(super)`。

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    sync::watch,
    task::JoinSet,
    time::{interval, sleep_until, Instant, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};

use super::{
    admission::CommandReceiver,
    failure::{DeliveryLedger, FailureLedger},
    freeze::{resolve_event, Resolved},
    prepare::prepare_event,
    recovery::{self, Delivery, RetryQueue, Submission},
    shutdown::{drain_cleanup, SendCounter, WorkerExit},
    split::{split, BatchLimits, PreparedEvent, RejectedEvent},
    stats::{Counters, SpoolFallbackReason},
    BatcherCommand, FlushAck, QueuedEvent,
};
use crate::client::LangfuseClient;
use crate::config::{BatcherConfig, TelemetryConfig};
use crate::error::LangfuseError;
use crate::memory::{slot_capacity, MemoryBudget};
use crate::payload::InputSnapshot;
use crate::snapshot::{InputSource, TelemetryEvent};
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::SpoolStore;

mod recover;

/// OTLP 编码计量入口（契约 §3.4）；不构造整棵 `Vec<OtelSpan>`。
type OtlpMeasure = fn(&[TelemetryEvent], usize) -> Result<u64, LangfuseError>;

/// 生产计量实现：`encoding::measure_otlp` 与写入端共用同一 emitter（只替换 sink）。
const OTLP_METER: OtlpMeasure = crate::encoding::measure_otlp;

/// 快照输出字节：Inline 按正文长度，Spooled 按落盘长度；未收敛的来源不计（尚无产出）。
fn snapshot_bytes(event: &TelemetryEvent) -> u64 {
    [event.input.as_ref(), event.output.as_ref()]
        .into_iter()
        .flatten()
        .map(|source| match source {
            InputSource::Ready(InputSnapshot::Inline(bytes)) => bytes.len() as u64,
            InputSource::Ready(InputSnapshot::Spooled(object)) => object.meta().stored_len,
            InputSource::Preparing(_) => 0,
        })
        .sum()
}

pub(super) struct BatchWorker {
    pub(super) client: Arc<LangfuseClient>,
    pub(super) budget: Arc<MemoryBudget>,
    pub(super) limits: BatchLimits,
    meter: OtlpMeasure,
    pub(super) buffer: VecDeque<QueuedEvent>,
    /// 已解析并计量的 FIFO 前缀，先于 `buffer` 交付；`Err` 是该条自身的准备/计量失败。
    pub(super) planned: VecDeque<(QueuedEvent, Result<PreparedEvent, LangfuseError>)>,
    buffered_bytes: usize,
    max_events: usize,
    max_batch_bytes: usize,
    pub(super) max_in_flight: usize,
    pub(super) prepare_max_in_flight: usize,
    /// 在途发送任务（live 批次与恢复/重试对象共用同一 JoinSet）。
    pub(super) in_flight: JoinSet<Submission>,
    /// 准备任务及其预约字节（`retained` + allowance），join 时归还进度账。
    pub(super) prepares: JoinSet<(u64, usize)>,
    /// 暂存端口；冻结来源与导出正文落盘都经它，配额/清理归属同一部署身份。
    pub(super) spool: Option<Arc<dyn SpoolStore>>,
    /// 清理注册表：唯一 worker 持有，冻结/导出对象各占一槽，实际 delete 只在 drain 里做。
    pub(super) registry: Option<CleanupRegistry>,
    pub(super) telemetry: TelemetryConfig,
    /// 持久 `retry_wait` 与恢复对象；claim 与正文由本队列持有到终态或移交。
    pub(super) retry: RetryQueue,
    /// 在途批次里最早的事件接受时刻；空在途时为 `i64::MAX`（年龄不靠猜测）。
    pub(super) oldest_in_flight_ms: i64,
    pub(super) dropped: Arc<AtomicUsize>,
    pub(super) failures: Arc<FailureLedger>,
    pub(super) counters: Arc<Counters>,
    pub(super) ledger: Arc<DeliveryLedger>,
}

impl BatchWorker {
    // 每个参数都是独立依赖句柄；打包成结构体只是把同一批依赖搬个位置，不多一层语义。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        client: LangfuseClient,
        config: &BatcherConfig,
        budget: Arc<MemoryBudget>,
        spool: Option<Arc<dyn SpoolStore>>,
        dropped: Arc<AtomicUsize>,
        failures: Arc<FailureLedger>,
        counters: Arc<Counters>,
        ledger: Arc<DeliveryLedger>,
    ) -> Result<Self, LangfuseError> {
        let limits = BatchLimits {
            max_event_bytes: config.max_event_bytes,
            max_batch_bytes: config.max_batch_bytes,
            // `max_request_bytes` 是解压后 JSON 上限（契约 §3.1），与批原文上限是两个独立限额。
            max_encoded_bytes: client.export_config().max_request_bytes,
            max_events: config.max_events,
        };
        split(&[], &limits)?;
        // 清理注册表只在真的能落盘时建立：`spool=None` 的部署没有磁盘产物可清理。
        let registry = match &spool {
            Some(store) => Some(CleanupRegistry::new(
                Arc::clone(store),
                Arc::clone(&budget),
                slot_capacity(config)?,
            )),
            None => None,
        };
        // 部署级降级原因只在构造期登记一次，运行期不逐次刷日志（契约 §6/§7）。
        if !config.telemetry.spool_enabled {
            counters.set_fallback_reason(SpoolFallbackReason::Disabled);
            info!("telemetry spool disabled by configuration; bounded in-memory mode");
        } else if spool.is_none() {
            counters.set_fallback_reason(SpoolFallbackReason::Unsupported);
            warn!("telemetry spool capability unavailable; bounded in-memory mode");
        }
        Ok(Self {
            client: Arc::new(client),
            budget,
            limits,
            meter: OTLP_METER,
            buffer: VecDeque::new(),
            buffered_bytes: 0,
            max_events: config.max_events,
            max_batch_bytes: config.max_batch_bytes,
            max_in_flight: config.max_in_flight,
            prepare_max_in_flight: config.telemetry.prepare_max_in_flight,
            in_flight: JoinSet::new(),
            prepares: JoinSet::new(),
            planned: VecDeque::new(),
            spool,
            registry,
            telemetry: config.telemetry.clone(),
            retry: RetryQueue::default(),
            oldest_in_flight_ms: i64::MAX,
            dropped,
            failures,
            counters,
            ledger,
        })
    }

    pub(super) async fn run(
        mut self,
        mut rx: CommandReceiver,
        mut closing: watch::Receiver<bool>,
        flush_interval: Duration,
    ) -> WorkerExit {
        // 恢复先于准入消费：同 route 的完整 Export 进入同一重试队列，由本 worker 续发。
        self.recover().await;
        self.publish_progress();
        let mut interval = interval(flush_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;
        // 先克隆唤醒点再进 select：分支前不得持有 receiver 的借用（`recv` 需要 `&mut`）。
        let prepare_progress = rx.prepare_progress();
        loop {
            // 每轮顶部重查准备队列：`notify_waiters` 不保存给未来等待者的许可。
            self.dispatch_prepares(&rx);
            if *closing.borrow() {
                break;
            }
            let retry_at = self.retry_wakeup();
            let wake = sleep_until(retry_at.unwrap_or_else(Instant::now));
            tokio::select! {
                _ = closing.changed() => break,
                _ = prepare_progress.notified() => {}
                _ = wake, if retry_at.is_some() => self.dispatch_retries().await,
                joined = self.prepares.join_next(), if !self.prepares.is_empty() => {
                    self.finish_prepare(joined);
                }
                result = self.in_flight.join_next(), if !self.in_flight.is_empty() => {
                    self.finish_send(result);
                    // 交付终态后正文句柄已归原清理槽，在这里下发有界 delete。
                    drain_cleanup(
                        self.registry.as_mut(), &self.ledger, self.spool.as_ref(), &self.counters,
                    ).await;
                }
                command = rx.recv(), if self.in_flight.len() < self.max_in_flight => match command {
                    Some(command) => self.process(command, &rx).await,
                    None => break,
                },
                _ = interval.tick(), if self.in_flight.len() < self.max_in_flight => {
                    let queued = self.queued_len();
                    if queued > 0 {
                        debug!("Batcher periodic flush: {queued} events ({flush_interval:?})");
                        self.pump_batches(None).await;
                    }
                    self.publish_progress();
                }
            }
        }
        rx.close();
        let handed = self.drain_after_close(&mut rx).await;
        if handed.local_pending_events > 0 {
            warn!(
                events = handed.local_pending_events,
                "telemetry events remained local at shutdown; reporting pending"
            );
        }
        self.publish_progress();
        if handed.handed_off_events > 0 {
            info!(
                events = handed.handed_off_events,
                "telemetry backlog handed off to the next owner"
            );
        }
        // 未收敛的条目、清理槽与正文随终态一起交回 Batcher：关闭期限到期或删除失败后
        // 仍由同一 owner 持有，下一次 shutdown 用它重入收敛，不丢 payload、不丢清理重试。
        WorkerExit::new(
            self.failures.shutdown_snapshot(),
            self.registry.take(),
            std::mem::take(&mut self.retry),
            self.spool.take(),
            Arc::clone(&self.ledger),
            Arc::clone(&self.counters),
        )
    }

    /// 命令分派：`Add` 入缓冲并按限额择机成批，`Flush` 收敛到给定边界后回报。
    ///
    /// 启动恢复不在本函数（见 `worker/recover.rs` 模块头，契约 §5.2）。
    pub(super) async fn process(&mut self, command: BatcherCommand, rx: &CommandReceiver) {
        match command {
            BatcherCommand::Add(queued) => {
                let bytes = super::budget::queued_bytes(&queued.event, self.limits.max_event_bytes)
                    .expect("admitted event satisfies its byte budget");
                self.buffered_bytes += bytes;
                self.buffer.push_back(queued);
                if self.queued_len() >= self.max_events
                    || self.buffered_bytes >= self.max_batch_bytes
                {
                    self.pump_batches(None).await;
                }
                self.publish_progress();
            }
            BatcherCommand::Flush { boundary, reply } => {
                self.converge(rx, boundary).await;
                super::stats::report_dropped(&self.dropped);
                let ack = FlushAck {
                    boundary,
                    failures: self.failures.snapshot(),
                    failed_snapshots: self.ledger.confirm_prepare_failures(boundary.prepare_seq),
                    counts: self.ledger.counts(),
                    prefix_unsettled: self.retry.unsettled_upto(boundary.event_seq),
                };
                if reply.send(ack).is_err() {
                    warn!("Batcher: flush ack receiver dropped");
                }
            }
        }
    }

    /// 组装并提交批次：解析计量 → 拆批 → 按 FIFO 顺序提交，容量用尽或前缀未收敛即停。
    pub(super) async fn pump_batches(&mut self, upto: Option<u64>) {
        loop {
            if self.in_flight.len() >= self.max_in_flight {
                return;
            }
            self.scan_prefix(upto);
            // 队首事件本身失败（准备失败/计量失败）：只终结该条并保留其余事件。
            if matches!(self.planned.front(), Some((_, Err(_)))) {
                let (queued, step) = self.planned.pop_front().expect("front item is planned");
                let Err(error) = step else {
                    unreachable!("front failure was checked before taking it");
                };
                self.fail_head(queued, error);
                continue;
            }
            let prepared: Vec<PreparedEvent> = self
                .planned
                .iter()
                .map_while(|(_, step)| step.as_ref().ok().copied())
                .collect();
            if prepared.is_empty() {
                // 队首来源未收敛或已达本次边界：不做计量，整条 FIFO 前缀保持等待。
                return;
            }
            let Ok(plan) = split(&prepared, &self.limits) else {
                error!("Batcher: batch split refused self-inconsistent limits");
                return;
            };
            // 按 FIFO 顺序合并批次与单条拒收；容量用尽即停，剩余项留给下次规划。
            let (mut batches, mut rejects) = (
                plan.batches.into_iter().peekable(),
                plan.rejected.into_iter().peekable(),
            );
            loop {
                if self.in_flight.len() >= self.max_in_flight {
                    return;
                }
                let next_batch = batches.peek().map(|batch| batch.range.start);
                let next_reject = rejects.peek().map(|item| item.index);
                match (next_batch, next_reject) {
                    (None, None) => break,
                    (start, Some(index)) if start.is_none_or(|start| index < start) => {
                        self.reject_front(rejects.next().expect("peeked rejection"));
                    }
                    _ => self.spawn_batch(batches.next().expect("peeked batch").len()),
                }
            }
        }
    }

    /// 解析并计量 `buffer` 前缀，按 FIFO 追加到 `planned`；遇未收敛、失败或边界即停（不重复计量）。
    fn scan_prefix(&mut self, upto: Option<u64>) {
        while let Some(front) = self.buffer.front_mut() {
            if upto.is_some_and(|boundary| front.seq > boundary) {
                return;
            }
            let step = match resolve_event(&mut front.event) {
                Resolved::Waiting => return,
                Resolved::Failed(error) => Err(error),
                Resolved::Ready => {
                    // 收敛快照按实际产出计入活跃快照字节，退役时归还（与 retire 同口径）。
                    let produced = snapshot_bytes(&front.event);
                    self.counters.add_active_snapshot_bytes(produced);
                    match prepare_event(&front.event, &self.limits, &self.meter) {
                        Ok(PreparedEvent::Waiting) => return,
                        Ok(prepared) => Ok(prepared),
                        Err(error) => Err(error),
                    }
                }
            };
            let failed = step.is_err();
            let queued = self.buffer.pop_front().expect("front item is queued");
            self.planned.push_back((queued, step));
            if failed {
                return;
            }
        }
    }

    /// 单条超限：只拒该条，其后事件继续成批；不伪造 Delivered。
    fn reject_front(&mut self, rejected: RejectedEvent) {
        let Some((queued, _)) = self.planned.pop_front() else {
            return;
        };
        self.retire(&queued.event);
        self.counters
            .rejected_events
            .fetch_add(1, Ordering::Relaxed);
        self.ledger.failed(1);
        self.ledger.terminal(1);
        debug!(
            seq = queued.seq,
            accepted_at_ms = queued.accepted_at_ms,
            error = %rejected.error(),
            "Batcher: oversized event rejected without affecting its batch"
        );
    }

    /// 队首事件自身失败（准备失败/计量失败）：只终结该条并保留其余事件。
    fn fail_head(&mut self, queued: QueuedEvent, error: LangfuseError) {
        self.retire(&queued.event);
        self.ledger.failed(1);
        self.ledger.terminal(1);
        error!(
            seq = queued.seq,
            accepted_at_ms = queued.accepted_at_ms,
            error = %error,
            "Batcher: telemetry event could not be prepared"
        );
    }

    /// 事件离开待交付集合：结算其队列字节占用与活跃快照字节。
    fn retire(&mut self, event: &TelemetryEvent) {
        let bytes = super::budget::queued_bytes(event, self.limits.max_event_bytes).unwrap_or(0);
        self.buffered_bytes = self.buffered_bytes.saturating_sub(bytes);
        self.counters
            .release_active_snapshot_bytes(snapshot_bytes(event));
    }

    /// 尚未离开 worker 的事件条数（已解析前缀 + 未解析尾部）。
    pub(super) fn queued_len(&self) -> usize {
        self.planned.len() + self.buffer.len()
    }

    fn spawn_batch(&mut self, count: usize) {
        let mut events = Vec::with_capacity(count);
        let mut accepted_at_ms = i64::MAX;
        let mut prefix_seq = u64::MAX;
        while events.len() < count {
            let (queued, _prepared) = self
                .planned
                .pop_front()
                .expect("planned batch range is available");
            accepted_at_ms = accepted_at_ms.min(queued.accepted_at_ms);
            prefix_seq = prefix_seq.min(queued.seq);
            self.retire(&queued.event);
            events.push(queued.event);
        }
        let count = events.len() as u64;
        // 导出对象跨阈值落盘与冻结来源共用同一注册表；只有纯内存部署才退回构造期注册。
        let cleanup = match (&self.spool, &self.registry) {
            (Some(_), Some(registry)) => match registry.try_register() {
                Ok(registration) => registration,
                Err(error) => {
                    self.fail_batch(events, count, LangfuseError::Spool(error));
                    return;
                }
            },
            _ => match crate::client::body::memory_registration(&self.budget) {
                Ok(registration) => registration,
                Err(error) => {
                    self.fail_batch(events, count, error);
                    return;
                }
            },
        };
        let context = match self.client.export_context(self.telemetry.spool_ttl) {
            Ok(context) => context,
            Err(error) => {
                self.fail_batch(events, count, error);
                return;
            }
        };
        let expires_at_ms = context.expires_at_ms;
        self.counters
            .submitted_batches
            .fetch_add(1, Ordering::Relaxed);
        self.counters
            .in_flight_batches
            .fetch_add(1, Ordering::Relaxed);
        self.ledger.spawn_task();
        let counter = SendCounter(Arc::clone(&self.counters));
        let client = Arc::clone(&self.client);
        let telemetry = self.telemetry.clone();
        let budget = Arc::clone(&self.budget);
        let spool = self.spool.clone();
        let counters = Arc::clone(&self.counters);
        self.track_in_flight(accepted_at_ms);
        self.in_flight.spawn(async move {
            let _counter = counter;
            let export = client.export_config().clone();
            let outcome = match recovery::encode_batch(
                events,
                telemetry,
                export.clone(),
                budget,
                spool,
                context,
                cleanup,
                counters,
            )
            .await
            {
                Ok(payload) => {
                    recovery::send_export(&client, payload, expires_at_ms, &export).await
                }
                Err(error) => Delivery::failed(error),
            };
            super::recovery::Submission {
                events: count,
                accepted_at_ms,
                expires_at_ms,
                persisted: false,
                prefix_seq: Some(prefix_seq),
                outcome,
            }
        });
    }

    pub(super) async fn drain_sends(&mut self) {
        while !self.in_flight.is_empty() {
            let joined = self.in_flight.join_next().await;
            self.finish_send(joined);
        }
    }

    /// 下一次重试唤醒时刻（单调时钟换算）；`None` 表示无需唤醒。
    fn retry_wakeup(&self) -> Option<Instant> {
        let wait_ms = (self.retry.next_due_ms()? - super::wall_ms()).max(0) as u64;
        Some(Instant::now() + Duration::from_millis(wait_ms))
    }

    pub(super) fn track_in_flight(&mut self, accepted_at_ms: i64) {
        self.oldest_in_flight_ms = self.oldest_in_flight_ms.min(accepted_at_ms);
    }

    /// 发布可观察的进度量：队列字节与最老积压年龄（含在途与重试条目）。
    pub(super) fn publish_progress(&self) {
        let mut oldest = self.oldest_in_flight_ms;
        let queued = [
            self.retry.oldest_accepted_ms(),
            self.planned
                .front()
                .map(|(queued, _)| queued.accepted_at_ms),
            self.buffer.front().map(|queued| queued.accepted_at_ms),
        ];
        for accepted in queued.into_iter().flatten() {
            oldest = oldest.min(accepted);
        }
        self.counters
            .publish_progress(self.buffered_bytes, (oldest != i64::MAX).then_some(oldest));
    }
}

// 端到端发送回归挂在 worker 侧：它驱动的是本模块的批次发送单元，native 暂存用例只在
// 有文件系统的目标上编译（WASM/emscripten 只保留有界内存路径）。
#[cfg(all(test, not(target_arch = "wasm32"), not(target_os = "emscripten")))]
#[path = "send_test.rs"]
mod tests;
