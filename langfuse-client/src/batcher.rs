mod admission;
mod failure;
mod shutdown;
mod worker;

use admission::Admission;
use failure::{FailureLedger, FlushSnapshot};
use shutdown::WorkerOwner;
use worker::BatchWorker;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::warn;

use crate::{
    config::{BackpressurePolicy, BatcherConfig},
    error::LangfuseError,
    types::IngestionEvent,
    LangfuseClient,
};

/// Batcher 内部命令（不导出）
#[allow(clippy::large_enum_variant)]
enum BatcherCommand {
    Add(IngestionEvent),
    Flush(oneshot::Sender<FlushSnapshot>),
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
    /// 准入丢弃计数；worker 每次 flush 后汇总输出并清零。
    dropped: Arc<AtomicUsize>,
    failures: Arc<FailureLedger>,
}

impl Batcher {
    /// 创建聚合器并启动唯一的后台事件处理 task。
    pub fn new(client: LangfuseClient, config: BatcherConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.max_events);
        let (admission, closing) = Admission::new(tx);
        let dropped = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(FailureLedger::default());
        let worker = BatchWorker::new(client, &config, Arc::clone(&dropped), Arc::clone(&failures));
        let handle = tokio::spawn(worker.run(rx, closing, config.flush_interval));
        Self {
            admission,
            worker: Mutex::new(WorkerOwner::Running(handle)),
            backpressure: config.backpressure,
            dropped,
            failures,
        }
    }

    /// 添加事件。DropNew/DropOldest 在命令队列满时拒绝新事件；Block 等待空位。
    /// 关闭开始后均返回 ChannelClosed；等待空位尚未提交的事件不属于排空集合。
    pub async fn add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        match self.backpressure {
            BackpressurePolicy::DropNew | BackpressurePolicy::DropOldest => self.try_add(event),
            BackpressurePolicy::Block => {
                let result = self.admission.send(BatcherCommand::Add(event)).await;
                self.report_rejection(result)
            }
        }
    }

    /// 同步非阻塞添加事件，按成功提交的顺序入队；队列满时返回 QueueFull。
    pub fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.report_rejection(self.admission.try_send(BatcherCommand::Add(event)))
    }

    fn report_rejection(&self, result: Result<(), LangfuseError>) -> Result<(), LangfuseError> {
        if let Err(error) = &result {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            warn!("Batcher event rejected: {error}");
        }
        result
    }

    /// 等待此前入队事件完成发送尝试，报告该确认点尚未被调用方观察的批次失败。
    ///
    /// 返回 Err 后只确认本次快照的失败水位；后续失败仍由下次 flush 报告。
    /// 取消等待或仅由后台发送 ack 不会确认错误。并发 flush 可观察到同一失败，
    /// 确认是幂等的；已观察的历史失败不会使后续干净的 flush 永久失败。
    /// HTTP 重试仍由 LangfuseClient 负责，错误摘要不包含事件或响应内容。
    /// 关闭期间及关闭后改为等待并返回同一个 shutdown 终态。
    pub async fn flush(&self) -> Result<(), LangfuseError> {
        let (tx, rx) = oneshot::channel();
        if self
            .admission
            .send(BatcherCommand::Flush(tx))
            .await
            .is_err()
        {
            return self.shutdown().await;
        }
        match rx.await {
            // No await between receipt and confirmation: a cancelled waiter
            // cannot consume a failure it never observed.
            Ok(snapshot) => self.failures.observe(snapshot),
            Err(_) => self.shutdown().await,
        }
    }

    /// 停止准入，排空已接受的事件并 join 唯一后台任务。
    ///
    /// 取消等待不取消 worker，也不取走 join handle；后续或并发调用继续等待同一任务。
    /// 返回值是固定终态：IngestionApi 表示 worker 已正常 join，但存在未观察的发送失败；
    /// WorkerJoinFailed 表示已取得 JoinError，worker 未正常排空（取消或 panic）。
    /// 错误摘要不包含 HTTP 响应或 panic 内容。重复调用返回相同终态，不重新发送事件。
    pub async fn shutdown(&self) -> Result<(), LangfuseError> {
        self.admission.close();
        self.worker.lock().await.join(&self.failures).await
    }

    /// 当前累计的准入丢弃事件数；worker 每次 flush 后清零。
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
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

#[cfg(test)]
#[path = "batcher_test.rs"]
mod tests;

#[cfg(test)]
#[path = "batcher_shutdown_test.rs"]
mod shutdown_tests;
