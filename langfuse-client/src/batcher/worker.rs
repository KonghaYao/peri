//! The sole worker owns buffering, FIFO barriers, and all bounded HTTP tasks.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    sync::watch,
    task::JoinSet,
    time::{interval, Duration, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};

use super::{
    admission::CommandReceiver,
    budget::event_bytes,
    failure::{FailureLedger, ShutdownSnapshot},
    stats::Counters,
    BatcherCommand,
};
use crate::{config::BatcherConfig, types::IngestionEvent, LangfuseClient, LangfuseError};

pub(super) struct BatchWorker {
    client: Arc<LangfuseClient>,
    buffer: VecDeque<IngestionEvent>,
    max_events: usize,
    dropped: Arc<AtomicUsize>,
    failures: Arc<FailureLedger>,
    counters: Arc<Counters>,
    buffered_bytes: usize,
    max_batch_bytes: usize,
    max_event_bytes: usize,
    max_in_flight: usize,
    in_flight: JoinSet<Result<(), LangfuseError>>,
}

struct SendCounter(Arc<Counters>);

impl Drop for SendCounter {
    fn drop(&mut self) {
        self.0.in_flight_batches.fetch_sub(1, Ordering::Relaxed);
    }
}

impl BatchWorker {
    pub(super) fn new(
        client: LangfuseClient,
        config: &BatcherConfig,
        dropped: Arc<AtomicUsize>,
        failures: Arc<FailureLedger>,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            // A capacity limit does not require reserving that many event-sized
            // allocations before the first event arrives.
            buffer: VecDeque::new(),
            max_events: config.max_events,
            dropped,
            failures,
            counters,
            buffered_bytes: 0,
            max_batch_bytes: config.max_batch_bytes,
            max_event_bytes: config.max_event_bytes,
            max_in_flight: config.max_in_flight,
            in_flight: JoinSet::new(),
        }
    }

    pub(super) async fn run(
        mut self,
        mut rx: CommandReceiver,
        mut closing: watch::Receiver<bool>,
        flush_interval: Duration,
    ) -> ShutdownSnapshot {
        let mut interval = interval(flush_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            if *closing.borrow() {
                break;
            }
            tokio::select! {
                _ = closing.changed() => break,
                _ = self.complete_one(), if !self.in_flight.is_empty() => {},
                command = rx.recv(), if self.in_flight.len() < self.max_in_flight => match command {
                    Some(command) => self.process(command).await,
                    None => break,
                },
                _ = interval.tick(), if self.in_flight.len() < self.max_in_flight => {
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
        while let Some(command) = rx.try_recv() {
            self.process(command).await;
        }
        if !self.buffer.is_empty() {
            info!(
                "Batcher shutting down, flushing {} remaining events",
                self.buffer.len()
            );
        }
        self.flush_buffer().await;
        self.drain_sends().await;
        Self::report_dropped(&self.dropped);
        self.failures.shutdown_snapshot()
    }

    async fn process(&mut self, command: BatcherCommand) {
        match command {
            BatcherCommand::Add(event) => {
                let bytes = event_bytes(&event, self.max_event_bytes)
                    .expect("admitted event satisfies its byte budget");
                if !self.buffer.is_empty()
                    && bytes > self.max_batch_bytes.saturating_sub(self.buffered_bytes)
                {
                    self.flush_buffer().await;
                }
                self.buffered_bytes += bytes;
                self.buffer.push_back(event);
                if self.buffer.len() >= self.max_events
                    || self.buffered_bytes >= self.max_batch_bytes
                {
                    self.flush_buffer().await;
                }
            }
            BatcherCommand::Flush(ack) => {
                self.flush_buffer().await;
                self.drain_sends().await;
                Self::report_dropped(&self.dropped);
                if ack.send(self.failures.snapshot()).is_err() {
                    warn!("Batcher: flush ack receiver dropped");
                }
            }
        }
    }

    async fn flush_buffer(&mut self) {
        if !self.buffer.is_empty() {
            while self.in_flight.len() >= self.max_in_flight {
                self.complete_one().await;
            }
            let events: Vec<IngestionEvent> = self.buffer.drain(..).collect();
            self.buffered_bytes = 0;
            let client = Arc::clone(&self.client);
            let limit = self.max_batch_bytes;
            self.counters
                .submitted_batches
                .fetch_add(1, Ordering::Relaxed);
            self.counters
                .in_flight_batches
                .fetch_add(1, Ordering::Relaxed);
            let counter = SendCounter(Arc::clone(&self.counters));
            self.in_flight.spawn(async move {
                let _counter = counter;
                client.ingest_with_limit(events, limit).await
            });
        }
        Self::report_dropped(&self.dropped);
    }

    async fn complete_one(&mut self) {
        let Some(result) = self.in_flight.join_next().await else {
            return;
        };
        self.counters
            .completed_batches
            .fetch_add(1, Ordering::Relaxed);
        let Ok(result) = result else {
            panic!("batch ingestion task did not complete normally");
        };
        if let Err(error) = result {
            self.failures.record_failure();
            self.counters.failed_batches.fetch_add(1, Ordering::Relaxed);
            if let LangfuseError::PartialSuccess { rejected_spans } = error {
                self.counters
                    .partially_rejected_batches
                    .fetch_add(1, Ordering::Relaxed);
                let _ = self.counters.rejected_spans.fetch_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |count| Some(count.saturating_add(rejected_spans)),
                );
            }
            error!("Batcher ingestion submission failed");
        }
    }

    async fn drain_sends(&mut self) {
        while !self.in_flight.is_empty() {
            self.complete_one().await;
        }
    }

    /// 输出丢弃汇总日志并清零计数（每次 flush 完成后调用）。
    ///
    /// 容量或字节预算耗尽时仅计数，批次提交、屏障及关闭时汇总。
    fn report_dropped(dropped: &AtomicUsize) {
        let n = dropped.swap(0, Ordering::Relaxed);
        if n > 0 {
            warn!(
                target: "langfuse::batcher",
                dropped = n,
                "Batcher 已丢弃 {} 条事件（容量、字节预算或关闭）",
                n
            );
        }
    }
}
