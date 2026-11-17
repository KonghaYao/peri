use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Formatter};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

/// 触发事件（由 CronScheduler 发送到 App）
// 3.0 批 2 波 1：协议类型归契约层（`peri_acp_types::cron::CronTrigger`）。
pub use peri_acp_types::cron::CronTrigger;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[path = "scheduler/cron_scheduler.rs"]
mod cron_scheduler;
#[cfg(test)]
use cron_scheduler::parse_cron;
pub use cron_scheduler::CronError;
pub use cron_scheduler::CronScheduler;
pub use cron_scheduler::CronSchedulerPortHandle;
pub use cron_scheduler::CronTask;
pub use cron_scheduler::MAX_CRON_TASKS;
