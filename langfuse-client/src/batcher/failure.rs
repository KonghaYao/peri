//! 交付账本与终态记账（契约 §3.6、§5.2）：每个提交单元的终态（交付/重试/待修复/失败）在这里结算，
//! flush 确认独立于部署累计失败，账本之外不发任何请求。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::task::JoinError;
use tracing::{debug, error, warn};

use super::recovery::{Delivery, RetryEntry, Submission};
use super::worker::BatchWorker;
use crate::error::LangfuseError;
use crate::snapshot::TelemetryEvent;

use crate::DeliveryCounts;

mod barrier;

pub(in crate::batcher) use barrier::{FailureLedger, FlushSnapshot, ShutdownSnapshot};

#[path = "failure/delivery_ledger.rs"]
mod delivery_ledger;
pub(super) use delivery_ledger::DeliveryLedger;

use delivery_ledger::submission_result;
