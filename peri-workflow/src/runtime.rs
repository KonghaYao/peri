//! WorkflowRuntime —— workflow 编排的领域控制接口（MCP adapter 背后的唯一控制面）。
//!
//! 设计依据：`docs/design/workflow-mcp.md` §3。MCP 对外契约（工具 / 资源 / 订阅）
//! 经 `peri-mcp-workflow` 的 handler 表达；Node wire（`workflow/start`、`agent/task`
//! 等）是本类型的实现细节。旧 middleware 链在迁移期经本类型复用同一份状态。

use std::fmt::{self, Debug, Formatter};
mod close_gate;
pub mod event_log;
mod events;
pub mod params;
mod tasks;

pub use close_gate::{AdmissionTicket, CloseGate};
pub use event_log::{BoundedEventLog, EventLogPage};
pub use events::{WorkflowRunEvent, WorkflowRunEventKind, WorkflowRunEventLog};
pub use tasks::{
    ClaimOutcome, PublishOutcome, SettleAdmission, SettleOutcome, TaskBaseline, WorkflowTaskEvent,
    WorkflowTaskEventKind, WorkflowTaskRecord, WorkflowTaskState, WorkflowTaskTable,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedReceiver;

use peri_acp_types::tasks::TaskManager;

use crate::journal::WorkflowJournalStore;
use crate::progress::{RunProgress, WorkflowProgressStore};
use crate::protocol::{AgentRunParams, AgentRunResult, ProgressEvent};
use crate::registry::{WorkflowTaskRegistry, WorkflowTaskResult};
use crate::runner::{
    AgentExecutor, AgentTaskPublish, AgentTaskSink, WorkflowInput, WorkflowRunner,
};

use events::RuntimeProgressObserver;

#[cfg(test)]
use workflow_runtime::RUN_EVENT_LOG_CAPACITY;

pub use workflow_runtime::WORKFLOW_CONSUMER_META_KEY;

pub use workflow_runtime::WorkflowResourceUpdate;

pub use workflow_runtime::WorkflowStartOutcome;

pub use workflow_runtime::RunExecutionBudget;

pub use workflow_runtime::WorkflowRuntime;

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

#[path = "runtime/workflow_runtime.rs"]
mod workflow_runtime;
