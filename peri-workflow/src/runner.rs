//! WorkflowRunner —— 公开运行入口、子进程所有权与取消收敛。
//!
//! P6 wire：Node → 宿主 `agent/task`（通知）经 [`AgentTaskSink`] 发布为任务表记录；
//! 宿主的终态提交（[`Self::deliver_agent_result`](WorkflowRunner::deliver_agent_result)）向 Node 投递 `agent/result`（通知）。

use std::fmt::{self, Debug, Formatter};
mod artifact;
mod run_limit_state;
use run_limit_state as limits;
mod message_loop;
mod run_protocol;
mod run_scope;
use run_scope as scope;
mod terminal;

use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::{JsExecutionHost, JsProcessSpec};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use message_loop::MessageLoop;

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::{AgentResultParams, AgentRunParams, AgentRunResult, WorkflowLimits};
use crate::rpc::RpcChannel;

pub(crate) use artifact::prepare_workflow_command;
pub(crate) use artifact::WorkflowCommand;
pub use limits::RunLimitBreach;
pub(crate) use limits::RunLimitReport;
pub(crate) use limits::RunLimitState;

// ─── Agent 回调 trait（3.0 批 2 波 1 迁入 peri-acp-types）────────────

pub use peri_acp_types::workflow::AgentExecutor;

pub use workflow_runner::AgentTaskPublish;

pub use workflow_runner::AgentTaskSink;

pub use workflow_runner::WorkflowInput;

pub use workflow_runner::WorkflowResult;

pub use workflow_runner::receive_workflow_result;

pub use workflow_runner::WorkflowRunner;

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;

#[path = "runner/workflow_runner.rs"]
mod workflow_runner;
