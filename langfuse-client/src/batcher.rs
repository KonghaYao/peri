//! 事件批量聚合器：有界准入、唯一 worker owner 与 FIFO 前缀屏障（契约 §3.6）。
//!
//! 队列只保存**有界来源/引用**（[`QueuedEvent`] 持有事件的原文 permit 与 input/output 句柄），
//! 不保存完整正文；文件引用按描述内存计费（[`budget::queued_bytes`]）。持有大对象的冻结准备
//! 阶段先经 [`Batcher::try_freeze`] 同步取得预算（先槽后额），IO 才由唯一 worker 执行。
//!
//! `event_seq`/`prepare_seq` 在同一个 admission 锁内各自单调递增；[`Batcher::flush`] 在入队时
//! 原子捕获两个水位，只等待该前缀的发送结果与准备收敛，后缀不并入前缀。

mod admission;
mod budget;
mod failure;
mod freeze;
mod prepare;
mod recovery;
mod result;
mod shutdown;
mod split;
mod stats;
mod worker;

use admission::{Admission, AdmissionOutcome};
use failure::{DeliveryLedger, FailureLedger, FlushSnapshot};
pub use result::{DeliveryCounts, FlushReport, ShutdownReport};
use shutdown::WorkerOwner;
use stats::Counters;
pub use stats::{BatcherStats, SpoolFallbackReason};
use worker::BatchWorker;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tracing::warn;

use crate::client::ExportConfig;
use crate::config::{BackpressurePolicy, BatcherConfig};
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::InputSnapshot;
use crate::snapshot::{InputSource, SnapshotSource, TelemetryEvent};
use crate::spool::SpoolStore;
use crate::types::IngestionEvent;
use crate::LangfuseClient;

/// batcher 内共用的墙上时钟毫秒；非负，准入、恢复、重试与统计同口径。
pub(super) fn wall_ms() -> i64 {
    peri_time::now_wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// 队列中的一条已准入事件（契约 §3.6）。
struct QueuedEvent {
    seq: u64,
    event: TelemetryEvent,
    accepted_at_ms: i64,
}

/// flush 捕获的双水位（契约 §3.6）。
#[derive(Debug, Clone, Copy)]
struct FlushBoundary {
    event_seq: u64,
    prepare_seq: u64,
}

/// 屏障 ack：捕获前缀的双水位、两类失败增量与**屏障时刻**重算的交付计数。
///
/// 契约 §3.6 要求屏障结果私有保存 `FlushBoundary` 与 `DeliveryCounts`；计数必须随 ack 返回，
/// 否则 `flush` 只能读调用方观察时刻的实时计数，会把捕获前缀**之后**的事件算进本次屏障。
struct FlushAck {
    boundary: FlushBoundary,
    /// 批次失败增量（[`failure::FailureLedger`] 的确认水位）。
    failures: FlushSnapshot,
    /// 准备失败增量；与 `failed_events` 分开记账，不伪造事件失败。
    failed_snapshots: u64,
    counts: DeliveryCounts,
    /// 捕获前缀内是否仍有驻留重试队列的条目（重试等待/待修复）；恢复条目不属于任何活前缀。
    prefix_unsettled: bool,
}

/// Batcher 内部命令（不导出）。
// `Add` 随事件整份移动（约 1.3 KiB）；装箱会给每个事件加一次分配，而通道本身有界。
#[allow(clippy::large_enum_variant)]
enum BatcherCommand {
    Add(QueuedEvent),
    Flush {
        boundary: FlushBoundary,
        reply: oneshot::Sender<FlushAck>,
    },
}

/// Langfuse 事件批量聚合器。
///
/// 后台 task 按 `max_events` 或 `flush_interval` 自动发送，支持手动 flush。
/// 部署/进程 owner 应在所有事件生产者结束后调用 [`Self::shutdown`]，等待排空及 join；
/// 单个 turn 只调用 [`Self::flush`]。Drop 仅尽力通知排空，不能等待后台任务结束。
pub struct Batcher {
    admission: Admission,
    worker: Mutex<WorkerOwner>,
    backpressure: BackpressurePolicy,
    /// 部署预算：唯一 worker、准入与冻结共用同一份身份。
    budget: Arc<MemoryBudget>,
    /// 冻结快照的输出 allowance（spool 阈值前缀 / 纯内存事件原文上界）。
    snapshot_allowance: usize,
    /// 准入丢弃计数；worker 每次 flush 后汇总输出并清零。
    dropped: Arc<AtomicUsize>,
    failures: Arc<FailureLedger>,
    counters: Arc<Counters>,
    ledger: Arc<DeliveryLedger>,
    /// 关闭宽限（契约 §5.2）：等待 worker 收敛的上界；到期只报告 Pending，保留 owner 供重入。
    shutdown_grace: Duration,
}

impl Batcher {
    /// 创建聚合器并启动唯一的后台事件处理 task。
    ///
    /// # Panics
    /// 配置容量无效或间隔为零时立即 panic；可恢复配置错误请使用 [`Self::try_new`]。
    /// 与 Tokio spawn 一样，必须在 Tokio runtime 内调用。
    pub fn new(
        client: LangfuseClient,
        config: BatcherConfig,
        spool: Option<Arc<dyn SpoolStore>>,
    ) -> Self {
        Self::try_new(client, config, spool).expect("invalid batcher configuration")
    }

    /// 在启动 worker 前验证配置并预留部署预算；无效容量或零间隔返回 Config 错误。
    /// 必须在 Tokio runtime 内调用。HTTP 重试仍仅由传入的 client 决定。
    pub fn try_new(
        client: LangfuseClient,
        config: BatcherConfig,
        spool: Option<Arc<dyn SpoolStore>>,
    ) -> Result<Self, LangfuseError> {
        config.validate()?;
        // 部署预算一次预留进度分区 P（`limit > P` 由 for_batcher 检查）；预算与拆批限额读
        // client 已生效的同一份 ExportConfig。两个公开配置独立构造出冲突时返回 Config，
        // 不任取一个（契约 §3.6）。
        let export = client.export_config();
        ensure_export_agrees(&config, export)?;
        let budget = MemoryBudget::for_batcher(&config, export)?;
        let snapshot_allowance =
            budget::snapshot_allowance(&config.telemetry, config.max_event_bytes);
        let (admission, rx, closing) = Admission::with_limits(&config);
        let dropped = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(FailureLedger::default());
        let counters = Arc::new(Counters::default());
        let ledger = Arc::new(DeliveryLedger::default());
        let worker = BatchWorker::new(
            client,
            &config,
            Arc::clone(&budget),
            spool,
            Arc::clone(&dropped),
            Arc::clone(&failures),
            Arc::clone(&counters),
            Arc::clone(&ledger),
        )?;
        let handle = tokio::spawn(worker.run(rx, closing, config.flush_interval));
        Ok(Self {
            admission,
            worker: Mutex::new(WorkerOwner::Running(handle)),
            backpressure: config.backpressure,
            budget,
            snapshot_allowance,
            dropped,
            failures,
            counters,
            ledger,
            shutdown_grace: config.telemetry.shutdown_grace,
        })
    }

    /// 部署预算身份；SessionLike 透传用，不允许外部替换预算。
    pub fn memory_budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }

    /// 冻结准入：同步计费并登记准备任务，IO 随唯一 worker 执行。
    ///
    /// 失败立即释放（不“先缓存再补票”），也不留下半登记：准备槽不足返回 `QueueFull`，
    /// retained/allowance 额度不足返回 `MemoryBudgetExceeded`，关闭后返回 `ChannelClosed`。
    pub fn try_freeze(
        &self,
        source: Arc<dyn SnapshotSource>,
    ) -> Result<InputSource, LangfuseError> {
        match self
            .admission
            .try_freeze(source, &self.budget, self.snapshot_allowance)
        {
            Ok(ticket) => Ok(InputSource::Preparing(ticket)),
            Err(error) => {
                // 该 generation 的冻结被拒：计数这一次拒收，不伪造 fallback 正文。
                self.counters
                    .rejected_events
                    .fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    /// 添加事件。DropNew 拒绝满队列；DropOldest 替换未受 flush 保护的最旧事件；Block 等待空位。
    /// 关闭开始后均返回 ChannelClosed；等待空位尚未提交的事件不属于排空集合。
    pub async fn add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        match self.backpressure {
            BackpressurePolicy::DropNew | BackpressurePolicy::DropOldest => self.try_add(event),
            BackpressurePolicy::Block => {
                let prepared = self.prepare_direct(event);
                let result = match prepared {
                    Ok(event) => self.admission.send(event).await,
                    Err(error) => Err(error),
                };
                if result.is_ok() {
                    // Block 只等待空位、不驱逐，成功即等价于 Accepted。
                    self.record_accepted(1);
                }
                self.report_rejection(result)
            }
        }
    }

    /// 同步非阻塞添加事件。DropOldest 可替换最后一个已准入 flush 之后的最旧 Add，
    /// 新事件始终追加在队尾；队列已满时，其他策略或无可驱逐事件会返回 QueueFull。
    pub fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        let result = self
            .prepare_direct(event)
            .and_then(|event| self.admit(event));
        self.report_rejection(result)
    }

    /// 同步非阻塞添加**已冻结**事件（`TelemetryEvent` 准入的唯一入口）。
    ///
    /// 与 [`Self::try_add`] 不是双编码路径：本入口服务提前取得 input/output 快照的真实用例，
    /// 事件 permit、Inline permit、Spooled 清理注册与 Preparing cell 的预算身份必须逐项与部署
    /// 预算 `Arc::ptr_eq`（金额相同不算同一预算），否则返回 `BudgetMismatch`。
    pub fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        let result = self.check_budget(&event).and_then(|()| self.admit(event));
        self.report_rejection(result)
    }

    /// 等待此前仍在队列中的事件完成发送尝试，报告该确认点尚未被调用方观察的批次失败。
    /// 已准入的 flush 保护其前缀不再被驱逐；准入前已被 DropOldest 替换的事件不在集合内。
    /// flush 等待容量、尚未准入时不建立保护前缀，也不恢复此前已驱逐的事件。
    ///
    /// 只有前缀内事件全部到达终态才是 `Complete`：前缀内仍有重试等待/待修复条目时返回
    /// `Pending`/`PersistedPending`（带计数快照），写盘成功不算交付。屏障只覆盖自己捕获的
    /// 双水位，后缀未交付不让本次屏障失败。
    ///
    /// 返回 Err 后只确认本次快照的失败水位；后续失败仍由下次 flush 报告。
    /// 取消等待或仅由后台发送 ack 不会确认错误。并发 flush 可观察到同一失败，
    /// 确认是幂等的；已观察的历史失败不会使后续干净的 flush 永久失败。
    /// 确认不抹去部署的累计失败，shutdown 仍会报告这些失败。
    /// HTTP 重试仍由 LangfuseClient 负责，错误摘要不包含事件或响应内容。
    /// 关闭期间及关闭后改为等待并返回同一个 shutdown 终态。
    pub async fn flush(&self) -> Result<FlushReport, LangfuseError> {
        let (tx, rx) = oneshot::channel();
        let boundary = match self.admission.flush_command(tx).await {
            Ok(boundary) => boundary,
            Err(_) => return self.closed_flush().await,
        };
        let ack = match rx.await {
            Ok(ack) => ack,
            Err(_) => return self.closed_flush().await,
        };
        // 屏障 ack 必须属于本次捕获的边界：前缀混淆会让本次 flush 报告后缀的终态。
        if ack.boundary.event_seq != boundary.event_seq
            || ack.boundary.prepare_seq != boundary.prepare_seq
        {
            return Err(LangfuseError::Config(
                "flush barrier ack does not match its captured boundary".into(),
            ));
        }
        // No await between receipt and confirmation: a cancelled waiter
        // cannot consume a failure it never observed.
        //
        // 准备失败优先：它携带完整 counts，且不伪造 failed_events；批次失败仍按旧
        // `IngestionApi` 摘要语义确认水位。
        if ack.failed_snapshots > 0 {
            return Err(LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: ack.failed_snapshots,
                counts: ack.counts,
            });
        }
        self.failures.observe(ack.failures)?;
        // 前缀内仍有驻留重试队列的条目（重试等待/待修复）：本前缀没有交付完成。驻留条目必然仍在
        // 待交付集合里（`pending_events > 0`），分类复用同一套交付终态规则；后缀的未交付事件不
        // 进入这条判定（`prefix_unsettled` 只覆盖前缀水位），干净的屏障仍不会被后缀拖成失败。
        if ack.prefix_unsettled {
            let counts = ack.counts;
            return Err(delivery_error(counts).unwrap_or(LangfuseError::Pending { counts }));
        }
        // 屏障只对自己的前缀作结论：`counts` 是诊断用的全局快照，后缀仍未交付（Pending）
        // 不属于本次前缀，不能让一次完成的屏障失败。
        Ok(FlushReport::Complete)
    }

    /// 停止准入，排空已接受的事件并 join 唯一后台任务。
    ///
    /// 取消等待不取消 worker，也不取走 join handle；后续或并发调用继续等待同一任务。
    /// 返回值是固定终态：Completed 表示全部事件已交付且完成清理；IngestionApi 表示 worker 已
    /// 正常 join、其生命周期内存在发送失败且无其他待报告状态，包括此前已由 flush 观察的失败；
    /// Pending/PersistedPending 表示仍有待交付事件，failed/cleanup 计数与待交付一并报告，
    /// DeliveryIncomplete 表示已无待交付但有待清理对象；WorkerJoinFailed 表示已取得 JoinError（取消或 panic）。
    /// 错误摘要不包含 HTTP 响应或 panic 内容。重复调用返回相同终态，不重新发送事件。
    /// 收敛等待受 `shutdown_grace` 限制：到期仍未 join 时返回带 `active_tasks` 的 `Pending`，
    /// 保留同一 owner 与保留的清理/移交状态，后续调用可重入并继续收敛（契约 §5.2）。
    pub async fn shutdown(&self) -> Result<ShutdownReport, LangfuseError> {
        self.admission.close();
        let mut worker = self.worker.lock().await;
        // 关闭期限：期限内借用**同一** owner 等待；到期不 abort、不取走句柄——join future 被取消
        // 只取消这一次等待，`Running` 里的任务继续由 owner 持有，后续 shutdown 可重入 join（D6）。
        let joined = match peri_time::timeout(self.shutdown_grace, worker.join()).await {
            Ok(joined) => joined,
            Err(_elapsed) => {
                let mut counts = self.ledger.counts();
                // 未 join 的 worker 自身就是仍在执行的 owner：active_tasks 至少为 1，否则
                // Pending 会与「无待交付、无活跃任务」的 DeliveryIncomplete 语义混淆。
                counts.active_tasks = counts.active_tasks.max(1);
                warn!(
                    grace_secs = self.shutdown_grace.as_secs(),
                    active_tasks = counts.active_tasks,
                    "Batcher shutdown grace elapsed; owner retained for a later join"
                );
                return Err(LangfuseError::Pending { counts });
            }
        };
        // worker 未正常结束比交付结果更根本，优先报告；其余先报带计数的交付终态，
        // 不因一条仅 HTTP 失败的摘要丢掉同期的 pending/failed 计数（契约 §3.6）。
        if let Err(LangfuseError::WorkerJoinFailed { cancelled }) = &joined {
            return Err(LangfuseError::WorkerJoinFailed {
                cancelled: *cancelled,
            });
        }
        // 重入收敛：join 后仍归本 owner 的清理槽与未收敛 payload 在这里再试一次（删除失败
        // 不换 owner、不丢槽），因此关闭期限到期或删除暂时失败的调用可以真正重入收敛。
        if let Some(retained) = worker.retained() {
            retained.converge().await;
        }
        // 收敛后计数不再增长（准入已关闭、worker 已 join），重复调用读到同一终态。
        let counts = self.ledger.counts();
        let failed_snapshots = self.ledger.prepare_failures();
        if failed_snapshots > 0 {
            return Err(LangfuseError::SnapshotPreparationFailed {
                failed_snapshots,
                counts,
            });
        }
        if let Some(error) = delivery_error(counts) {
            return Err(error);
        }
        joined?;
        self.failures.shutdown_snapshot().result()?;
        Ok(ShutdownReport::Complete)
    }

    /// 当前累计的准入丢弃事件数；worker 每次 flush 后清零。
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 累计计数不随周期汇总清零；字段分别读取，不是并发事务快照。
    pub fn stats(&self) -> BatcherStats {
        self.counters.snapshot()
    }

    /// 直接事件入口：立即进入同一 `TelemetryEvent` 准入实现，不另建编码路径。
    fn prepare_direct(&self, event: IngestionEvent) -> Result<TelemetryEvent, LangfuseError> {
        TelemetryEvent::try_new(event, None, None, &self.budget)
    }

    /// 准入成功：事件进入待交付集合，计入接收计数（唯一的正向提交点）。
    fn record_accepted(&self, events: u64) {
        self.ledger.accepted(events);
        self.counters
            .accepted_events
            .fetch_add(events as usize, Ordering::Relaxed);
    }

    /// 队列准入与交付账本的唯一提交点。
    fn admit(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        match self.admission.try_add(event, self.backpressure) {
            Ok(AdmissionOutcome::Accepted) => {
                self.record_accepted(1);
                Ok(())
            }
            Ok(AdmissionOutcome::ReplacedOldest(count)) => {
                // 被驱逐的事件离开待交付集合，不伪造 Delivered；新事件进入集合。
                self.ledger.terminal(count as u64);
                self.dropped.fetch_add(count, Ordering::Relaxed);
                self.counters
                    .evicted_events
                    .fetch_add(count, Ordering::Relaxed);
                self.record_accepted(1);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 与部署预算逐项核对事件及其全部来源的预算身份（契约 §3.6）。
    ///
    /// 判定规则与 `snapshot::TelemetryEvent::try_new` 一致；该处目前未导出访问器，故此处重复
    /// 同一份 `Arc::ptr_eq` 规则（已列入跨流改动请求：`snapshot.rs` 提供 `belongs_to`）。
    fn check_budget(&self, event: &TelemetryEvent) -> Result<(), LangfuseError> {
        let foreign = || LangfuseError::BudgetMismatch;
        if !Arc::ptr_eq(event.permit.budget(), &self.budget) {
            return Err(foreign());
        }
        for source in [event.input.as_ref(), event.output.as_ref()]
            .into_iter()
            .flatten()
        {
            let same = match source {
                InputSource::Preparing(ticket) => Arc::ptr_eq(&ticket.cell.budget, &self.budget),
                InputSource::Ready(InputSnapshot::Inline(bytes)) => {
                    Arc::ptr_eq(bytes.permit().budget(), &self.budget)
                }
                InputSource::Ready(InputSnapshot::Spooled(object)) => {
                    Arc::ptr_eq(object.cleanup.slot.budget(), &self.budget)
                }
            };
            if !same {
                return Err(foreign());
            }
        }
        Ok(())
    }

    fn report_rejection(&self, result: Result<(), LangfuseError>) -> Result<(), LangfuseError> {
        if result.is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.counters
                .rejected_events
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// 关闭期间及关闭后的 flush 只等待并透传同一个 shutdown 终态。
    async fn closed_flush(&self) -> Result<FlushReport, LangfuseError> {
        self.shutdown().await.map(|_| FlushReport::Complete)
    }

    #[cfg(test)]
    async fn worker_is_joined(&self) -> bool {
        matches!(*self.worker.lock().await, WorkerOwner::Joined(_))
    }
}

impl Drop for Batcher {
    fn drop(&mut self) {
        // Nonblocking best effort. Explicit shutdown retains the join guarantee;
        // dropping the handle here detaches without aborting admitted work.
        self.admission.close();
    }
}

/// client 的 `ExportConfig` 与 `BatcherConfig.telemetry` 必须对内容编码与 wire 上限取得一致
/// （契约 §3.6）：两者是公开可独立构造的配置，冲突时返回 `Config`，不任取一个。
fn ensure_export_agrees(
    config: &BatcherConfig,
    export: &ExportConfig,
) -> Result<(), LangfuseError> {
    if export.compression != config.telemetry.compression
        || export.max_wire_bytes != config.telemetry.max_wire_bytes
    {
        return Err(LangfuseError::Config(
            "client export encoding/wire limit conflicts with the batcher telemetry config".into(),
        ));
    }
    Ok(())
}

/// 交付终态分类（契约 §3.6）。
///
/// Pending 要求 `pending_events > 0 || active_tasks > 0`；全部待交付事件都已持久接受时是
/// PersistedPending（本阶段没有可恢复 Export，故不会同时出现）；已无待交付与活跃任务但仍有
/// 待清理对象时是 DeliveryIncomplete。分类不掩盖其他计数：错误携带完整计数快照。
fn delivery_error(counts: DeliveryCounts) -> Option<LangfuseError> {
    if counts.pending_events > 0 {
        return Some(
            if counts.persisted_pending_events == counts.pending_events {
                LangfuseError::PersistedPending { counts }
            } else {
                LangfuseError::Pending { counts }
            },
        );
    }
    if counts.active_tasks > 0 {
        return Some(LangfuseError::Pending { counts });
    }
    if counts.cleanup_pending_objects > 0 {
        return Some(LangfuseError::DeliveryIncomplete { counts });
    }
    None
}

#[cfg(test)]
#[path = "batcher/spool_test.rs"]
mod spool_tests;

#[cfg(test)]
#[path = "batcher/recovery_test.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "batcher/pending_test.rs"]
mod pending_tests;

#[cfg(test)]
#[path = "batcher_test.rs"]
mod tests;

#[cfg(test)]
#[path = "batcher_shutdown_test.rs"]
mod shutdown_tests;

#[cfg(test)]
#[path = "batcher_concurrency_test.rs"]
mod concurrency_tests;
