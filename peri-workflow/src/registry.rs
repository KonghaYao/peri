//! WorkflowTaskRegistry —— 管理活跃 workflow run，并发限制 + 完成通知。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}

impl WorkflowTaskResult {
    /// 格式化为 `<system-reminder>` 块，注入 ReAct 循环。
    pub fn to_notification(&self) -> String {
        let short_id = &self.run_id[..8.min(self.run_id.len())];
        let error_line = self
            .error
            .as_ref()
            .map(|e| format!("Error: {}\n", e))
            .unwrap_or_default();
        format!(
            "<system-reminder>\n\
            Workflow {} {}.\n\
            Workflow: {}\n\
            Status: {:?}\n\
            Duration: {}ms\n\
            Agents: {}\n\
            Tool calls: {}\n\
            Run ID: {}\n\
            {}\
            Result saved to .claude/workflow-runs/{}/state.json\n\
            Use Read tool to view full results.\n\
            </system-reminder>",
            short_id,
            if self.success { "completed" } else { "failed" },
            self.workflow_name,
            self.status,
            self.duration_ms,
            self.agent_count,
            self.tool_calls_count,
            self.run_id,
            error_line,
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
        let _ = self.notification_tx.send(result);
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
        run.child_handle.abort();
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
mod tests {
    use super::*;

    fn make_registry() -> (
        WorkflowTaskRegistry,
        tokio::sync::broadcast::Receiver<WorkflowTaskResult>,
    ) {
        let (tx, rx) = tokio::sync::broadcast::channel(32);
        (WorkflowTaskRegistry::new(tx), rx)
    }

    fn make_run(id: &str) -> WorkflowRun {
        let (kill_tx, _kill_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        WorkflowRun {
            run_id: id.into(),
            workflow_name: "test".into(),
            script_preview: "...".into(),
            status: WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: handle,
            kill_tx: Some(kill_tx),
        }
    }

    #[tokio::test]
    async fn test_register_and_active_count() {
        let (reg, _rx) = make_registry();
        assert_eq!(reg.active_count(), 0);
        reg.register(make_run("r1")).unwrap();
        assert_eq!(reg.active_count(), 1);
    }

    #[tokio::test]
    async fn test_concurrent_limit() {
        let (reg, _rx) = make_registry();
        reg.register(make_run("r1")).unwrap();
        reg.register(make_run("r2")).unwrap();
        reg.register(make_run("r3")).unwrap();
        let result = reg.register(make_run("r4"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_complete_sends_notification() {
        let (reg, mut rx) = make_registry();
        reg.register(make_run("r1")).unwrap();
        reg.complete(
            "r1",
            WorkflowTaskResult {
                run_id: "r1".into(),
                workflow_name: "test".into(),
                success: true,
                status: WorkflowRunStatus::Completed,
                duration_ms: 100,
                agent_count: 3,
                tool_calls_count: 5,
                error: None,
            },
        );
        let result = rx.recv().await.unwrap();
        assert_eq!(result.run_id, "r1");
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_complete_retains_history_with_status() {
        let (reg, _rx) = make_registry();
        reg.register(make_run("r1")).unwrap();
        assert_eq!(reg.active_count(), 1);

        reg.complete(
            "r1",
            WorkflowTaskResult {
                run_id: "r1".into(),
                workflow_name: "test".into(),
                success: true,
                status: WorkflowRunStatus::Completed,
                duration_ms: 100,
                agent_count: 3,
                tool_calls_count: 5,
                error: None,
            },
        );

        // complete 后 active_count 归零
        assert_eq!(reg.active_count(), 0);

        // 但 list_runs 仍保留记录（状态更新为 Completed）
        let runs = reg.list_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, WorkflowRunStatus::Completed);
    }

    #[test]
    fn test_notification_includes_error_when_failed() {
        // failed 通知必须包含真实 error 文本
        let result = WorkflowTaskResult {
            run_id: "run-xyz-1234".into(),
            workflow_name: "haiku-smoke-test".into(),
            success: false,
            status: WorkflowRunStatus::Failed,
            duration_ms: 58,
            agent_count: 0,
            tool_calls_count: 0,
            error: Some("parallel thunk #0 failed: t is not a function".into()),
        };
        let notification = result.to_notification();
        assert!(notification.contains("failed"), "通知应含 failed");
        assert!(
            notification.contains("parallel thunk #0 failed"),
            "通知应含真实 error 文本，实际：{notification}"
        );
    }

    #[test]
    fn test_notification_omits_error_line_when_completed() {
        // completed 时通知不应出现 Error: 行
        let result = WorkflowTaskResult {
            run_id: "run-ok-12345".into(),
            workflow_name: "test".into(),
            success: true,
            status: WorkflowRunStatus::Completed,
            duration_ms: 1000,
            agent_count: 2,
            tool_calls_count: 0,
            error: None,
        };
        let notification = result.to_notification();
        assert!(
            !notification.contains("Error:"),
            "completed 不应有 Error 行"
        );
    }
}
