//! The sole ingestion worker owns HTTP, buffering, and FIFO command processing.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    sync::{mpsc, watch},
    time::{interval, Duration},
};
use tracing::{debug, error, info, warn};

use super::{
    failure::{FailureLedger, FlushSnapshot},
    BatcherCommand,
};
use crate::{
    config::{BackpressurePolicy, BatcherConfig},
    types::IngestionEvent,
    LangfuseClient,
};

pub(super) struct BatchWorker {
    client: Arc<LangfuseClient>,
    buffer: VecDeque<IngestionEvent>,
    max_events: usize,
    backpressure: BackpressurePolicy,
    dropped: Arc<AtomicUsize>,
    failures: Arc<FailureLedger>,
}

impl BatchWorker {
    pub(super) fn new(
        client: LangfuseClient,
        config: &BatcherConfig,
        dropped: Arc<AtomicUsize>,
        failures: Arc<FailureLedger>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            buffer: VecDeque::with_capacity(config.max_events),
            max_events: config.max_events,
            backpressure: config.backpressure,
            dropped,
            failures,
        }
    }

    pub(super) async fn run(
        mut self,
        mut rx: mpsc::Receiver<BatcherCommand>,
        mut closing: watch::Receiver<bool>,
        flush_interval: Duration,
    ) -> FlushSnapshot {
        let mut interval = interval(flush_interval);
        interval.tick().await;
        loop {
            if *closing.borrow() {
                break;
            }
            tokio::select! {
                _ = closing.changed() => break,
                command = rx.recv() => match command {
                    Some(command) => self.process(command).await,
                    None => break,
                },
                _ = interval.tick() => {
                    if !self.buffer.is_empty() {
                        debug!("Batcher periodic flush: {} events (interval: {:?})", self.buffer.len(), flush_interval);
                        self.flush_buffer().await;
                    }
                }
            }
        }
        rx.close();
        // Admission is closed before the signal. No producer can commit after
        // this point. Drain committed commands only: recv().await could wait on
        // a reserved permit belonging to a suspended, unpolled producer.
        while let Ok(command) = rx.try_recv() {
            self.process(command).await;
        }
        if !self.buffer.is_empty() {
            info!(
                "Batcher shutting down, flushing {} remaining events",
                self.buffer.len()
            );
        }
        self.flush_buffer().await;
        self.failures.snapshot()
    }

    async fn process(&mut self, command: BatcherCommand) {
        match command {
            BatcherCommand::Add(event) => {
                // Preserve the existing buffer policy; queue overflow policy is
                // independently enforced at admission.
                if self.buffer.len() >= self.max_events
                    && self.backpressure == BackpressurePolicy::DropOldest
                    && self.buffer.pop_front().is_some()
                {
                    warn!(target: "langfuse::batcher", "DropOldest: 弹出最旧事件以容纳新事件");
                }
                self.buffer.push_back(event);
                if self.buffer.len() >= self.max_events {
                    self.flush_buffer().await;
                }
            }
            BatcherCommand::Flush(ack) => {
                self.flush_buffer().await;
                if ack.send(self.failures.snapshot()).is_err() {
                    warn!("Batcher: flush ack receiver dropped");
                }
            }
        }
    }

    async fn flush_buffer(&mut self) {
        Self::flush(&self.client, &mut self.buffer, &self.failures).await;
        Self::report_dropped(&self.dropped);
    }

    /// 执行一次 flush：将 buffer 中的事件通过原生 Ingestion 端点发送到 Langfuse API
    async fn flush(
        client: &LangfuseClient,
        buffer: &mut std::collections::VecDeque<IngestionEvent>,
        failures: &FailureLedger,
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
                failures.record_failure();
                error!("Batcher native ingestion flush failed");
            }
        }
    }

    /// 输出丢弃汇总日志并清零计数（每次 flush 完成后调用）。
    ///
    /// S5.2：`flush`（HTTP + 重试）await 期间 run_loop 无法消费命令通道，
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
}
