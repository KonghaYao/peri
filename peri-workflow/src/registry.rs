//! WorkflowTaskRegistry —— 管理活跃 workflow run，并发限制 + 完成通知。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::progress::PhaseSummary;

/// Registry 层错误。
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("Maximum {0} concurrent workflows reached")]
    ConcurrentLimit(usize),
    #[error("Workflow {0} not found")]
    NotFound(String),
}

/// Workflow run 状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

/// 单个 workflow run 记录。
pub struct WorkflowRun {
    pub run_id: String,
    pub workflow_name: String,
    pub script_preview: String,
    pub status: WorkflowRunStatus,
    pub started_at: std::time::Instant,
    pub child_handle: tokio::task::JoinHandle<()>,
    pub kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Workflow 完成后通过 broadcast channel 推送的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTaskResult {
    pub run_id: String,
    pub workflow_name: String,
    pub success: bool,
    pub status: WorkflowRunStatus,
    pub duration_ms: u64,
    pub agent_count: usize,
    pub tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_summaries: Vec<PhaseSummary>,
}

impl WorkflowTaskResult {
    /// 格式化为 `<system-reminder>` 块，含 phase breakdown。
    pub fn to_notification(&self) -> String {
        let success_msg = if self.success { "completed" } else { "failed" };

        let mut phase_lines = String::new();
        if !self.phase_summaries.is_empty() {
            for s in &self.phase_summaries {
                let token_info = if s.token_count > 0 {
                    format!(", {} tokens", s.token_count)
                } else {
                    String::new()
                };
                let dur_info = if let Some(d) = s.duration_ms {
                    format!(", {}ms", d)
                } else {
                    String::new()
                };
                phase_lines.push_str(&format!(
                    "- {}: {} agents{}{}\n",
                    s.name, s.agent_count, token_info, dur_info
                ));
            }
        }

        format!(
            "<system-reminder>\n\
            Workflow '{}' {}. ({}ms, {} agents, {} tool calls)\n\
            {}Results saved to .claude/workflow-runs/{}/state.json\n\
            Use Read tool to view full results.\n\
            </system-reminder>",
            self.workflow_name,
            success_msg,
            self.duration_ms,
            self.agent_count,
            self.tool_calls_count,
            phase_lines,
            self.run_id,
        )
    }
}

/// 管理活跃 workflow run，并发限制 + 完成通知。
pub struct WorkflowTaskRegistry {
    runs: parking_lot::Mutex<HashMap<String, WorkflowRun>>,
    notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
    max_concurrent: usize,
}

impl WorkflowTaskRegistry {
    /// 创建 registry，默认最大并发 3。
    pub fn new(notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>) -> Self {
        Self {
            runs: parking_lot::Mutex::new(HashMap::new()),
            notification_tx,
            max_concurrent: 3,
        }
    }

    /// 设置最大并发数（builder）。
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max;
        self
    }

    /// 获取 notification sender 的克隆。
    pub fn notification_tx(&self) -> tokio::sync::broadcast::Sender<WorkflowTaskResult> {
        self.notification_tx.clone()
    }

    /// 当前正在运行的 workflow 数量。
    pub fn active_count(&self) -> usize {
        let runs = self.runs.lock();
        runs.values()
            .filter(|r| r.status == WorkflowRunStatus::Running)
            .count()
    }

    /// 注册一个新的 workflow run，超出并发限制返回错误。
    pub fn register(&self, run: WorkflowRun) -> Result<(), RegistryError> {
        let mut runs = self.runs.lock();
        let active = runs
            .values()
            .filter(|r| r.status == WorkflowRunStatus::Running)
            .count();
        if active >= self.max_concurrent {
            return Err(RegistryError::ConcurrentLimit(self.max_concurrent));
        }
        runs.insert(run.run_id.clone(), run);
        Ok(())
    }

    /// 标记 workflow 完成，更新状态（保留历史记录），并发送通知。
    /// broadcast send 在无 subscriber 时返回错误，忽略即可。
    pub fn complete(&self, run_id: &str, result: WorkflowTaskResult) {
        let mut runs = self.runs.lock();
        if let Some(run) = runs.get_mut(run_id) {
            run.status = result.status.clone();
        }
        if let Err(e) = self.notification_tx.send(result) {
            warn!(target: "workflow", run_id = %run_id, error = %e, "registry: notification send failed (no subscribers)");
        }
    }

    /// 终止指定 workflow，移除记录并发送 kill 信号。
    pub fn kill(&self, run_id: &str) -> Result<(), RegistryError> {
        let run = self
            .runs
            .lock()
            .remove(run_id)
            .ok_or_else(|| RegistryError::NotFound(run_id.into()))?;
        if let Some(kill_tx) = run.kill_tx {
            let _ = kill_tx.send(());
        }
        // kill_tx 触发 runner 的 kill 分支（runner.rs:425），其中完成所有清理：
        // workflow/kill RPC → child.kill().await → state.json → done_tx.send()
        // → active_channels.remove() → progress_store.cleanup_completed()
        // 不在此处 abort child_handle，避免中断 runner 清理路径。
        Ok(())
    }

    /// 列出所有 workflow run 的摘要信息。
    pub fn list_runs(&self) -> Vec<(String, WorkflowRunStatus, String)> {
        let runs = self.runs.lock();
        runs.values()
            .map(|r| (r.run_id.clone(), r.status.clone(), r.workflow_name.clone()))
            .collect()
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
