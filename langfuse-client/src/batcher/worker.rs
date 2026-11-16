//! 唯一 worker：有界准备队列、FIFO 前缀屏障、启动恢复与重试续发，以及全部有界发送任务。
//!
//! 批次一律经流式编码产生 [`ExportPayload`]，再交 [`recovery::send_export`]：冻结 input/output 的
//! 正文随 payload 上线，恢复对象与 live 批次走同一条路径；只有 `Delivered` 是交付，只写盘未送达的
//! 事件留在 `pending`/`persisted` 账本里等待续发或移交。`Retryable` 持久写回 `retry_wait` 后由同一
//! worker 到期续发（首次时间/TTL 不变），一次调度一个唤醒。
//!
//! 跨文件 `impl BatchWorker`：准备任务收敛与关闭排空在 [`super::shutdown`]，交付终态记账在
//! [`super::failure`]；本模块字段对 `batcher` 内可见，方法按需要 `pub(super)`。

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::interval;
use tokio::time::sleep_until;
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use super::admission::CommandReceiver;
use super::failure::DeliveryLedger;
use super::failure::FailureLedger;
use super::freeze::resolve_event;
use super::freeze::Resolved;
use super::prepare::prepare_event;
use super::recovery;
use super::recovery::Delivery;
use super::recovery::RetryQueue;
use super::recovery::Submission;
use super::shutdown::drain_cleanup;
use super::shutdown::SendCounter;
use super::shutdown::WorkerExit;
use super::split::split;
use super::split::BatchLimits;
use super::split::PreparedEvent;
use super::split::RejectedEvent;
use super::stats::Counters;
use super::stats::SpoolFallbackReason;
use super::BatcherCommand;
use super::FlushAck;
use super::QueuedEvent;
use crate::client::LangfuseClient;
use crate::config::{BatcherConfig, TelemetryConfig};
use crate::error::LangfuseError;
use crate::memory::{slot_capacity, MemoryBudget};
use crate::payload::InputSnapshot;
use crate::snapshot::{InputSource, TelemetryEvent};
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::SpoolStore;

mod recover;

// 端到端发送回归挂在 worker 侧：它驱动的是本模块的批次发送单元，native 暂存用例只在
// 有文件系统的目标上编译（WASM/emscripten 只保留有界内存路径）。
#[cfg(all(test, not(target_arch = "wasm32"), not(target_os = "emscripten")))]
#[path = "send_test.rs"]
mod tests;

#[path = "worker/batch_worker.rs"]
mod batch_worker;

pub(super) use batch_worker::BatchWorker;
