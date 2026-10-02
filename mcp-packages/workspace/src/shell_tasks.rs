//! Workspace-owned Bash task state exposed through the MCP Tasks extension.
//!
//! The legacy shell executor still implements process execution. This owner
//! deliberately has no session callback: completion stays with the Workspace
//! service and can be queried after a client disconnects.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use peri_acp_types::{
    event::BackgroundTaskResult,
    tasks::{BgShellHandle, BgTaskKind, OnBgCompleteFn, TaskManager, TaskShutdownReport},
};
use rmcp::{
    model::{
        CallToolResult, ContentBlock, DetailedTask, GetTaskResult, Task, TaskPayload, TaskStatus,
    },
    ErrorData as McpError,
};

#[derive(Clone)]
pub(crate) struct ShellTasks {
    manager: Arc<dyn TaskManager>,
    records: Arc<Mutex<HashMap<String, DetailedTask>>>,
    updates: tokio::sync::broadcast::Sender<DetailedTask>,
    owner_references: Arc<()>,
}

impl ShellTasks {
    pub(crate) fn new() -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(64);
        Self {
            manager: Arc::new(peri_mcp_common::create_local_task_manager()),
            records: Arc::new(Mutex::new(HashMap::new())),
            updates,
            owner_references: Arc::new(()),
        }
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<DetailedTask> {
        self.updates.subscribe()
    }

    pub(crate) fn manager(&self) -> Arc<dyn TaskManager> {
        Arc::clone(&self.manager)
    }

    pub(crate) fn completion_callback(&self) -> OnBgCompleteFn {
        let records = Arc::clone(&self.records);
        let updates = self.updates.clone();
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            if kind != BgTaskKind::Shell {
                return;
            }
            let mut records = records.lock();
            let record = records.entry(result.task_id.clone()).or_insert_with(|| {
                let now = chrono::Utc::now().to_rfc3339();
                DetailedTask::new(
                    Task::new(&result.task_id, TaskStatus::Working, &now, &now),
                    TaskPayload::Working,
                )
            });
            if record.task.status.is_terminal() {
                return;
            }
            record.task.last_updated_at = chrono::Utc::now().to_rfc3339();
            let text = if result.success {
                "Background shell command completed."
            } else {
                "Background shell command failed."
            };
            record.task.status_message = Some(text.into());
            let mut output = CallToolResult::success(vec![ContentBlock::text(format!(
                "{text}\n{}",
                result.output
            ))]);
            output.is_error = Some(!result.success);
            output.structured_content = serde_json::to_value(result).ok();
            let result = serde_json::to_value(output)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            record.payload = TaskPayload::Completed { result };
            record.task.status = TaskStatus::Completed;
            let _ = updates.send(record.clone());
        })
    }

    pub(crate) async fn spawn(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
    ) -> Result<Task, McpError> {
        let manager = self.manager();
        let callback = self.completion_callback();
        let handle: BgShellHandle = tokio::task::spawn_blocking(move || {
            manager.spawn_shell(command, cwd, timeout_ms, Some(callback))
        })
        .await
        .map_err(|_| McpError::internal_error("shell task launch failed", None))?
        .map_err(|_| McpError::internal_error("shell task launch failed", None))?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut record = DetailedTask::new(
            Task::new(&handle.task_id, TaskStatus::Working, &now, &now)
                .with_status_message("Background shell command is running.")
                .with_poll_interval_ms(1000),
            TaskPayload::Working,
        );
        // A fast command can complete before spawn_shell returns. Keep its
        // terminal result rather than overwriting it with a working seed.
        let mut records = self.records.lock();
        if let Some(existing) = records.get(&handle.task_id) {
            record = existing.clone();
        } else {
            records.insert(handle.task_id, record.clone());
        }
        Ok(record.task)
    }

    /// Register a foreground call that the Bash executor already promoted to
    /// its owned background manager. The callback may have won the race.
    pub(crate) fn track_promoted(&self, task_id: &str) -> Task {
        let now = chrono::Utc::now().to_rfc3339();
        let record = DetailedTask::new(
            Task::new(task_id, TaskStatus::Working, &now, &now)
                .with_status_message("Foreground timeout promoted to background execution.")
                .with_poll_interval_ms(1000),
            TaskPayload::Working,
        );
        self.records
            .lock()
            .entry(task_id.to_string())
            .or_insert(record)
            .task
            .clone()
    }

    pub(crate) fn get(&self, task_id: &str) -> Result<GetTaskResult, McpError> {
        self.records
            .lock()
            .get(task_id)
            .cloned()
            .map(GetTaskResult::new)
            .ok_or_else(|| McpError::invalid_params("unknown task", None))
    }

    pub(crate) fn cancel(&self, task_id: &str) -> Result<(), McpError> {
        {
            let records = self.records.lock();
            let record = records
                .get(task_id)
                .ok_or_else(|| McpError::invalid_params("unknown task", None))?;
            if record.task.status.is_terminal() {
                return Ok(());
            }
        }
        // The manager may invoke completion callbacks while cancelling. Do
        // not hold the task record mutex across this call.
        let cancelled = self.manager.cancel(task_id);
        let mut records = self.records.lock();
        let record = records
            .get_mut(task_id)
            .ok_or_else(|| McpError::invalid_params("unknown task", None))?;
        if record.task.status.is_terminal() {
            return Ok(());
        }
        cancelled.map_err(|_| McpError::internal_error("shell task cancellation failed", None))?;
        record.task.status = TaskStatus::Cancelled;
        record.task.status_message = Some("Background shell cancellation requested.".into());
        record.task.last_updated_at = chrono::Utc::now().to_rfc3339();
        record.payload = TaskPayload::Cancelled;
        let _ = self.updates.send(record.clone());
        Ok(())
    }

    pub(crate) fn update(&self, task_id: &str) -> Result<(), McpError> {
        self.records
            .lock()
            .contains_key(task_id)
            .then_some(())
            .ok_or_else(|| McpError::invalid_params("unknown task", None))
    }

    pub(crate) async fn shutdown(&self) -> TaskShutdownReport {
        self.manager.shutdown().await
    }
}

impl Drop for ShellTasks {
    fn drop(&mut self) {
        // Emergency best effort only. Hosts must call shutdown() and await
        // process cleanup before their runtime stops.
        if Arc::strong_count(&self.owner_references) == 1 {
            self.manager.cancel_all();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use rmcp::model::{TaskPayload, TaskStatus};

    use super::ShellTasks;

    #[tokio::test]
    async fn shell_completion_is_queryable_after_client_like_owner_clone_is_dropped() {
        let dir = tempfile::tempdir().expect("workspace");
        let owner = ShellTasks::new();
        let second_connection = owner.clone();
        let task = owner
            .spawn(
                "printf 'workspace-task-ok'".into(),
                dir.path().to_string_lossy().into_owned(),
                None,
            )
            .await
            .expect("start shell task");
        drop(owner);

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = second_connection
                    .get(&task.task_id)
                    .expect("task remains owned");
                if result.task.status().is_terminal() {
                    break result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("shell must complete");
        assert_eq!(result.task.status(), TaskStatus::Completed);
        let TaskPayload::Completed { result } = result.task.payload else {
            panic!("terminal task must carry CallToolResult")
        };
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
        assert!(result.get("structuredContent").is_some());
        assert!(second_connection.get("unknown-task").is_err());
        assert_eq!(
            second_connection.shutdown().await,
            peri_acp_types::tasks::TaskShutdownReport::Complete
        );
    }

    #[tokio::test]
    async fn cancellation_is_scoped_to_known_task_id() {
        let dir = tempfile::tempdir().expect("workspace");
        let owner = ShellTasks::new();
        let task = owner
            .spawn(
                "sleep 30".into(),
                dir.path().to_string_lossy().into_owned(),
                None,
            )
            .await
            .expect("start shell task");
        assert!(owner.cancel("not-this-task").is_err());
        owner.cancel(&task.task_id).expect("cancel task");
        assert_eq!(
            owner
                .get(&task.task_id)
                .expect("cancelled task")
                .task
                .status(),
            TaskStatus::Cancelled
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
                .await
                .expect("shutdown must await process cleanup"),
            peri_acp_types::tasks::TaskShutdownReport::Complete
        );
    }

    #[tokio::test]
    async fn cancelling_after_completion_preserves_terminal_result() {
        let dir = tempfile::tempdir().expect("workspace");
        let owner = ShellTasks::new();
        let task = owner
            .spawn(
                "printf done".into(),
                dir.path().to_string_lossy().into_owned(),
                None,
            )
            .await
            .expect("start task");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if owner
                    .get(&task.task_id)
                    .expect("task")
                    .task
                    .status()
                    .is_terminal()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("task deadline");
        owner
            .cancel(&task.task_id)
            .expect("terminal cancel is idempotent");
        assert_eq!(
            owner.get(&task.task_id).expect("task result").task.status(),
            TaskStatus::Completed
        );
        assert_eq!(
            owner.shutdown().await,
            peri_acp_types::tasks::TaskShutdownReport::Complete
        );
    }
}
