//! WorkflowTool — 旧 middleware 链的 LLM 工具（deferred），委托 [`WorkflowRuntime`]。
//!
//! MCP 迁移双轨期保留：MCP 面（`mcp__workflow__Workflow`）经 `WorkflowMcpServer`
//! 提供同一份 `WorkflowRuntime` 状态；装配切换批次（P5）后本工具与旧链一并删除。
//! 工具立即返回 run_id，workflow 在后台执行；完成后经 notification channel 通知。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::tools::BaseTool;
use serde_json::Value;

use crate::runner::WorkflowInput;
use crate::runtime::{params, WorkflowRuntime, WorkflowStartOutcome};

/// Workflow 工具 — 启动 workflow（fire-and-forget）。
pub struct WorkflowTool {
    runtime: Arc<WorkflowRuntime>,
}

impl WorkflowTool {
    pub fn new(runtime: Arc<WorkflowRuntime>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &Arc<WorkflowRuntime> {
        &self.runtime
    }

    /// 启动一个已准备好的 run（供旧 middleware 的 resume 复用）。
    pub async fn start_run(&self, wf_input: WorkflowInput) -> Result<WorkflowStartOutcome, String> {
        self.runtime.start_prepared_run(wf_input).await
    }
}

#[async_trait]
impl BaseTool for WorkflowTool {
    fn name(&self) -> &str {
        "Workflow"
    }

    fn description(&self) -> &str {
        params::START_TOOL_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        params::start_tool_parameters()
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_acp_types::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let outcome = self.runtime.start_run(input).await?;
        Ok(outcome.message())
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
