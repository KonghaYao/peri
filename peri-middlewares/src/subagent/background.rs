use std::collections::HashMap;

use peri_agent::agent::BackgroundTaskResult;
use thiserror::Error;
use tracing::warn;

/// 后台任务注册表错误（结构化，取代 String 错误）
///
/// 参考已有 `lsp/tool.rs:LspToolError` 模式：实现 `std::error::Error`，
/// 调用方可通过 `?` 自动转 `Box<dyn Error>` / `anyhow::Error`。
#[derive(Debug, Error)]
pub enum BackgroundRegistryError {
    #[error("Maximum {0} concurrent background tasks reached")]
    ConcurrentLimit(usize),
    #[error("Task {0} not found")]
    TaskNotFound(String),
}

/// 后台任务状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
}

/// 后台任务信息（注册表条目）
pub struct BackgroundTask {
    pub id: String,
    pub agent_name: String,
    pub prompt_summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: std::time::Instant,
    pub abort_handle: tokio::task::JoinHandle<()>,
}

/// 后台任务注册中心
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<BackgroundTaskResult>,
    max_concurrent: usize,
}

impl BackgroundTaskRegistry {
    pub fn new(notification_tx: tokio::sync::mpsc::UnboundedSender<BackgroundTaskResult>) -> Self {
        Self {
            tasks: parking_lot::Mutex::new(HashMap::new()),
            notification_tx,
            max_concurrent: 3,
        }
    }

    /// 当前运行中的任务数
    pub fn active_count(&self) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running))
            .count()
    }

    /// 注册新任务，超出上限返回 Err。
    /// 单次持锁完成计数检查 + 插入，消除 concurrent register 的 TOCTOU 窗口。
    pub fn register(&self, task: BackgroundTask) -> Result<(), BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        let active = tasks
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running))
            .count();
        if active >= self.max_concurrent {
            return Err(BackgroundRegistryError::ConcurrentLimit(
                self.max_concurrent,
            ));
        }
        tasks.insert(task.id.clone(), task);
        Ok(())
    }

    /// 任务完成时调用：更新状态 + 推送通知
    pub fn complete(&self, task_id: &str, result: BackgroundTaskResult) {
        // 持锁：更新状态 + 清理所有非 Running 任务，防止 JoinHandle 长期驻留内存
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = if result.success {
                BackgroundTaskStatus::Completed
            } else {
                BackgroundTaskStatus::Failed
            };
        }
        tasks.retain(|_, t| matches!(t.status, BackgroundTaskStatus::Running));
        drop(tasks);

        if self.notification_tx.send(result).is_err() {
            warn!(
                task_id = %task_id,
                "background task complete: failed to send notification (channel closed)"
            );
        }
    }

    /// 获取所有任务状态（UI 使用）
    pub fn list_tasks(&self) -> Vec<(String, BackgroundTaskStatus, String)> {
        self.tasks
            .lock()
            .values()
            .map(|t| (t.id.clone(), t.status.clone(), t.prompt_summary.clone()))
            .collect()
    }

    /// 取消指定任务
    pub fn cancel(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.remove(task_id) {
            task.abort_handle.abort();
            Ok(())
        } else {
            Err(BackgroundRegistryError::TaskNotFound(task_id.to_string()))
        }
    }

    /// 清理已完成的任务
    pub fn cleanup_completed(&self) {
        self.tasks
            .lock()
            .retain(|_, t| matches!(t.status, BackgroundTaskStatus::Running));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("background_test.rs");
}
