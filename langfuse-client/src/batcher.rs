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

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
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

#[path = "batcher/batcher.rs"]
#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod batcher;
pub(super) use batcher::wall_ms;
pub use batcher::Batcher;
use batcher::BatcherCommand;
use batcher::FlushAck;
use batcher::FlushBoundary;
use batcher::QueuedEvent;
