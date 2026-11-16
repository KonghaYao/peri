//! 恢复与重试：`Retryable` 持久写回 `retry_wait` 由同一 worker 到期续发（首次时间与 TTL 不变）；
//! 只有 `Delivered` 是交付。启动扫描与清理登记见子模块 [`scan`]（契约 §5.2、§7 P4a）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::warn;

use super::stats::Counters;
use crate::client::{export, send, ExportConfig, LangfuseClient};
use crate::config::TelemetryConfig;
use crate::encoding::{BudgetedWriter, ExportContext};
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{ExportPayload, PayloadBody};
use crate::snapshot::TelemetryEvent;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    CleanupRegistration, ObjectClaim, ObjectMeta, SpoolError, SpoolState, SpoolStore,
};

mod scan;

pub(in crate::batcher) use scan::*;

#[path = "recovery/recovery.rs"]
#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and retains the descriptive name of the multi-type implementation leaf."
)]
mod recovery;
pub(super) use recovery::RetryEntry;

pub(super) use recovery::HandOff;

pub(super) use recovery::Delivery;
pub(super) use recovery::RetryQueue;
pub(super) use recovery::Submission;

pub(super) use recovery::encode_batch;
pub(super) use recovery::send_export;

pub(super) use recovery::expire_entry;
