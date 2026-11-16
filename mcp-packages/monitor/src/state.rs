//! In-memory monitor task registry and command runner.
//!
//! v1 only owns command sources: one shell command per task, line-oriented
//! stdout/stderr capture, and a bounded in-memory event ring buffer. Task state
//! is never persisted. After a process restart or instance replacement every old
//! task id is unknown.
//!
//! Task ownership is the initiating session identity carried by
//! `_meta["peri/taskOwner"]`. A caller with an owner sees only that owner's
//! tasks. A caller without an owner (local standalone use) is unscoped,
//! matching the Workspace task-owner contract: it sees every task in the
//! instance, including tasks owned by another session.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use parking_lot::Mutex;
use peri_process::ProcessTree;
use regex::Regex;
use serde::Serialize;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::buffer::{
    capture_lines, EventAdmission, EventBuffer, EventPage, MonitorEvent, MAX_READ_BYTES,
    MAX_READ_EVENTS, MAX_TASK_BYTES, MAX_TASK_EVENTS, MIN_TASK_BYTES,
};
use crate::buffer_budget::{BufferBudget, INSTANCE_EVENT_BYTES};

#[path = "retention.rs"]
mod retention;

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;

#[cfg(test)]
#[path = "retention_test.rs"]
mod retention_tests;

#[path = "state/monitor_state.rs"]
mod monitor_state;
pub use monitor_state::DEFAULT_DEADLINE_MS;
pub use monitor_state::DEFAULT_MAX_BYTES;
pub use monitor_state::DEFAULT_MAX_EVENTS;
pub use monitor_state::DEFAULT_READ_BYTES;
pub use monitor_state::DEFAULT_READ_LIMIT;
pub use monitor_state::MAX_DEADLINE_MS;
pub use monitor_state::MAX_RETAINED_TASKS;
pub use monitor_state::TERMINAL_RETENTION;

pub use monitor_state::CleanupReport;
pub use monitor_state::MonitorStatus;
pub use monitor_state::ReadResult;
pub use monitor_state::StartRequest;
pub use monitor_state::StartedMonitor;
pub use monitor_state::TaskSnapshot;
#[cfg(test)]
use monitor_state::TaskState;
pub use monitor_state::MAX_ALERT_PATTERN_CHARS;
pub use monitor_state::MAX_LABEL_BYTES;
pub use monitor_state::MAX_REASON_BYTES;

#[cfg(test)]
pub(crate) use monitor_state::resolve_start;
#[cfg(test)]
use monitor_state::AlertCounters;
pub use monitor_state::MonitorError;
pub use monitor_state::MonitorState;
use monitor_state::MonitorTask;
#[cfg(test)]
use monitor_state::TaskInner;

pub(crate) use monitor_state::validate_read_limits;
