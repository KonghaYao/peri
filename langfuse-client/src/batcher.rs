use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use tokio::{
    sync::{mpsc, oneshot},
    time::{interval, Duration},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::{BackpressurePolicy, BatcherConfig},
    error::LangfuseError,
    types::IngestionEvent,
    LangfuseClient,
};

/// Batcher 内部命令（不导出）
#[allow(clippy::large_enum_variant)]
enum BatcherCommand {
    /// 添加事件到待发送队列
    Add(IngestionEvent),
    /// 手动 flush：发送当前队列中的所有事件，完成后通过 oneshot 通知调用方
    Flush(oneshot::Sender<()>),
    /// 关闭后台 task（先 flush 剩余事件再退出）
    Shutdown,
}

/// Langfuse 事件批量聚合器
///
/// 通过后台 tokio task 异步收集事件，按 `max_events`（定量）或 `flush_interval`（定时）
/// 自动发送到 Langfuse API。支持手动 flush 和两种背压策略。
pub struct Batcher {
    tx: mpsc::Sender<BatcherCommand>,
    backpressure: BackpressurePolicy,
    /// 因命令通道满/关闭而被丢弃的事件计数（add/try_add 侧累加；
    /// run_loop 每次 flush 完成后汇总输出并清零——S5.2 丢弃可观测性）
    dropped: Arc<AtomicUsize>,
}

impl Batcher {
    /// 创建新的 Batcher 实例，同时启动后台事件处理 task
    pub fn new(client: LangfuseClient, config: BatcherConfig) -> Self {
        let client = Arc::new(client);
        let (tx, rx) = mpsc::channel(config.max_events);
        let backpressure = config.backpressure;

        let batch_client = Arc::clone(&client);
        let max_events = config.max_events;
        let flush_interval = config.flush_interval;
        let dropped = Arc::new(AtomicUsize::new(0));
        let run_dropped = Arc::clone(&dropped);

        let _handle = tokio::spawn(async move {
            Self::run_loop(
                batch_client,
                rx,
                max_events,
                flush_interval,
                backpressure,
                run_dropped,
            )
            .await;
        });

        Self {
            tx,
            backpressure,
            dropped,
        }
    }

    /// 后台事件处理循环
    async fn run_loop(
        client: Arc<LangfuseClient>,
        mut rx: mpsc::Receiver<BatcherCommand>,
        max_events: usize,
        flush_interval: Duration,
        backpressure: BackpressurePolicy,
        dropped: Arc<AtomicUsize>,
    ) {
        let mut buffer: std::collections::VecDeque<IngestionEvent> =
            std::collections::VecDeque::with_capacity(max_events);
        let mut interval = interval(flush_interval);
        interval.tick().await;

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    match cmd {
                        Some(BatcherCommand::Add(event)) => {
                            // DropOldest：buffer 满时弹出最旧事件，为新事件腾出空间
                            if buffer.len() >= max_events
                                && backpressure == BackpressurePolicy::DropOldest
                            {
                                if let Some(_dropped) = buffer.pop_front() {
                                    warn!(
                                        target: "langfuse::batcher",
                                        "DropOldest: 弹出最旧事件以容纳新事件"
                                    );
                                }
                            }
                            buffer.push_back(event);
                            if buffer.len() >= max_events {
                                Self::do_flush(&client, &mut buffer).await;
                                Self::report_dropped(&dropped);
                            }
                        }
                        Some(BatcherCommand::Flush(ack)) => {
                            Self::do_flush(&client, &mut buffer).await;
                            Self::report_dropped(&dropped);
                            if ack.send(()).is_err() {
                                warn!("Batcher: flush ack receiver dropped");
                            }
                        }
                        Some(BatcherCommand::Shutdown) | None => {
                            if !buffer.is_empty() {
                                info!(
                                    "Batcher shutting down, flushing {} remaining events",
                                    buffer.len()
                                );
                                Self::do_flush(&client, &mut buffer).await;
                            }
                            Self::report_dropped(&dropped);
                            return;
                        }
                    }
                }
                _ = interval.tick() => {
                    if !buffer.is_empty() {
                        debug!(
                            "Batcher periodic flush: {} events (interval: {:?})",
                            buffer.len(),
                            flush_interval
                        );
                        Self::do_flush(&client, &mut buffer).await;
                        Self::report_dropped(&dropped);
                    }
                }
            }
        }
    }

    /// 执行一次 flush：将 buffer 中的事件通过原生 Ingestion 端点发送到 Langfuse API
    async fn do_flush(
        client: &LangfuseClient,
        buffer: &mut std::collections::VecDeque<IngestionEvent>,
    ) {
        if buffer.is_empty() {
            return;
        }

        let events: Vec<IngestionEvent> = buffer.drain(..).collect();
        debug!("Batcher flushing {} events via OTLP", events.len());

        match client.ingest(events).await {
            Ok(()) => {
                debug!("Batcher OTLP flush successful");
            }
            Err(_) => {
                error!("Batcher native ingestion flush failed");
            }
        }
    }

    /// 输出丢弃汇总日志并清零计数（每次 flush 完成后调用）。
    ///
    /// S5.2：`do_flush`（HTTP + 重试）await 期间 run_loop 无法消费命令通道，
    /// DropNew/DropOldest 在通道满时会丢弃事件；本函数保证"已丢弃 N 条"
    /// 至少在每个 flush 周期后可见，避免静默丢失。
    fn report_dropped(dropped: &AtomicUsize) {
        let n = dropped.swap(0, Ordering::Relaxed);
        if n > 0 {
            warn!(
                target: "langfuse::batcher",
                dropped = n,
                "Batcher 已丢弃 {} 条事件（上一 flush 周期内命令通道满/关闭）",
                n
            );
        }
    }

    /// 添加事件到批量队列
    ///
    /// DropNew/DropOldest：使用 try_send 非阻塞发送，channel 满时 DropNew 直接丢弃，
    /// DropOldest 由 run_loop 在 buffer 层面弹出最旧事件。
    /// Block：使用 send 阻塞等待，直到 channel 有空位。
    pub async fn add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        let cmd = BatcherCommand::Add(event);
        match self.backpressure {
            BackpressurePolicy::DropNew | BackpressurePolicy::DropOldest => {
                self.tx.try_send(cmd).map_err(|e| match e {
                    mpsc::error::TrySendError::Full(_) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                        let policy_name = if self.backpressure == BackpressurePolicy::DropOldest {
                            "DropOldest"
                        } else {
                            "DropNew"
                        };
                        warn!(
                            "Batcher queue full, dropping event ({} policy)",
                            policy_name
                        );
                        LangfuseError::QueueFull
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                        warn!("Batcher channel closed, event dropped");
                        LangfuseError::ChannelClosed
                    }
                })
            }
            BackpressurePolicy::Block => self.tx.send(cmd).await.map_err(|_| {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                warn!("Batcher channel closed during send");
                LangfuseError::ChannelClosed
            }),
        }
    }

    /// 同步添加事件到批量队列（非阻塞，支持 DropNew/DropOldest 背压策略）
    ///
    /// 保证事件按调用顺序入队，适用于需要严格顺序的场景（如父 span 必须在子 span 之前）。
    pub fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        let cmd = BatcherCommand::Add(event);
        self.tx.try_send(cmd).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                warn!(
                    "Batcher queue full, dropping event ({} policy)",
                    if self.backpressure == BackpressurePolicy::DropOldest {
                        "DropOldest"
                    } else {
                        "DropNew"
                    }
                );
                LangfuseError::QueueFull
            }
            mpsc::error::TrySendError::Closed(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                warn!("Batcher channel closed, event dropped");
                LangfuseError::ChannelClosed
            }
        })
    }

    /// 手动触发 flush，等待所有待发送事件发送完毕
    pub async fn flush(&self) -> Result<(), LangfuseError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(BatcherCommand::Flush(tx)).await.map_err(|_| {
            warn!("Batcher channel closed, cannot flush");
            LangfuseError::ChannelClosed
        })?;
        rx.await.map_err(|_| {
            warn!("Batcher dropped flush acknowledgment");
            LangfuseError::ChannelClosed
        })
    }

    /// 当前累计的丢弃事件数（通道满/关闭导致；run_loop 每次 flush 后清零）。
    /// 供调用方/测试观测丢弃量——S5.2 慢 flush 期间 DropNew 丢事件的可观测性。
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Batcher {
    fn drop(&mut self) {
        // 发送 Shutdown 命令，后台任务会 flush 剩余事件后自行退出
        // 不调用 abort()：abort 会立即取消任务，导致缓冲区中的事件丢失
        let shutdown_cmd = BatcherCommand::Shutdown;
        if self.tx.try_send(shutdown_cmd).is_err() {
            debug!("Batcher Drop: channel already closed, background task may have exited");
        }
        // handle 不显式 abort：后台任务在处理完 Shutdown 后自行结束
        // Drop handle 会使 JoinHandle detach，任务继续运行到完成
    }
}

#[cfg(test)]
#[path = "batcher_test.rs"]
mod tests;
