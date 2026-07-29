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
    #[error("Kind concurrent limit reached: {kind} ({current}/{limit})")]
    KindConcurrentLimit {
        kind: String,
        current: usize,
        limit: usize,
    },
}

/// 后台任务类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgTaskKind {
    Shell,
    Agent,
    Workflow,
}

/// 后台任务取消句柄（按 kind 分发取消逻辑）
#[derive(Debug)]
pub enum BgCancelHandle {
    /// bg agent：取消 tokio task
    Abort(tokio::task::AbortHandle),
    /// workflow：通过 oneshot 通知 workflow runner kill
    Kill(Option<tokio::sync::oneshot::Sender<()>>),
    /// bg shell：OS 进程 kill
    Pid(u32),
}

/// 后台任务信息（注册表条目）
pub struct BackgroundTask {
    pub id: String,
    pub agent_name: String,
    pub prompt_summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: std::time::Instant,
    /// 任务创建时间（chrono UTC），用于 list_tasks_full().started_at 返回真实时间
    pub chrono_started_at: chrono::DateTime<chrono::Utc>,
    /// 任务类型
    pub kind: BgTaskKind,
    /// 按 kind 分发的取消句柄
    pub cancel_handle: BgCancelHandle,
    /// OS 进程 PID（仅 bg shell 有效）
    pub pid: Option<u32>,
    /// 输出预览（completed 时写入，最多 500 字符）
    pub output_preview: Option<String>,
}

/// 后台任务状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
}

/// 后台任务信息 DTO（序列化用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BgTaskInfo {
    pub task_id: String,
    pub kind: BgTaskKind,
    pub summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub pid: Option<u32>,
    pub output_preview: Option<String>,
}

/// Registry → ACP 层事件桥接
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BgRegistryEvent {
    Started {
        task_id: String,
        kind: BgTaskKind,
        summary: String,
        started_at: String,
    },
    Completed {
        task_id: String,
        success: bool,
        output_preview: String,
        duration_ms: u64,
        result: BackgroundTaskResult,
    },
    Cancelled {
        task_id: String,
        reason: String,
    },
}

/// 后台任务注册中心
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
    /// ACP 事件推送通道（由 executor 在 run_session_loop 注入）
    event_sender: parking_lot::RwLock<Option<tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>>>,
    session_id: parking_lot::RwLock<String>,
}

impl Default for BackgroundTaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundTaskRegistry {
    pub const SHELL_LIMIT: usize = 5;
    pub const AGENT_LIMIT: usize = 3;
    pub const WORKFLOW_LIMIT: usize = 3;

    pub fn new() -> Self {
        Self {
            tasks: parking_lot::Mutex::new(HashMap::new()),
            event_sender: parking_lot::RwLock::new(None),
            session_id: parking_lot::RwLock::new(String::new()),
        }
    }

    /// 设置 ACP 事件推送通道（由 executor 在 run_session_loop 调用）
    pub fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        *self.event_sender.write() = Some(sender);
        *self.session_id.write() = session_id;
    }

    /// 清除 ACP 事件推送通道（session 结束时调用）
    pub fn clear_event_sender(&self) {
        *self.event_sender.write() = None;
        self.session_id.write().clear();
    }

    /// 当前运行中的任务数
    pub fn active_count(&self) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running))
            .count()
    }

    /// 按类型统计运行中任务数
    pub fn count_by_kind(&self, kind: BgTaskKind) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running) && t.kind == kind)
            .count()
    }

    /// 按类型注册新任务（独立上限）
    pub fn register_with_kind(&self, task: BackgroundTask) -> Result<(), BackgroundRegistryError> {
        let limit = match task.kind {
            BgTaskKind::Shell => Self::SHELL_LIMIT,
            BgTaskKind::Agent => Self::AGENT_LIMIT,
            BgTaskKind::Workflow => Self::WORKFLOW_LIMIT,
        };

        let kind = task.kind;
        let task_id = task.id.clone();
        let summary = task.prompt_summary.clone();

        let mut tasks = self.tasks.lock();
        let current = tasks
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running) && t.kind == kind)
            .count();
        if current >= limit {
            let kind_str = match kind {
                BgTaskKind::Shell => "shell",
                BgTaskKind::Agent => "agent",
                BgTaskKind::Workflow => "workflow",
            };
            return Err(BackgroundRegistryError::KindConcurrentLimit {
                kind: kind_str.to_string(),
                current,
                limit,
            });
        }

        tasks.insert(task.id.clone(), task);
        drop(tasks);

        // 推送 BgTaskStarted 事件
        self.push_event(BgRegistryEvent::Started {
            task_id,
            kind,
            summary,
            started_at: chrono::Utc::now().to_rfc3339(),
        });

        Ok(())
    }

    /// 任务完成时调用：更新状态 + 推送通知
    pub fn complete(&self, task_id: &str, result: BackgroundTaskResult) {
        tracing::info!(
            task_id = %task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] registry.complete() called"
        );
        let duration_ms = result.duration_ms;
        let success = result.success;
        let output_preview: String = result.output.chars().take(500).collect();

        // 持锁：更新状态 + 清理所有非 Running 任务，防止 JoinHandle 长期驻留内存
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = if result.success {
                BackgroundTaskStatus::Completed
            } else {
                BackgroundTaskStatus::Failed
            };
            task.output_preview = Some(output_preview.clone());
        }
        tasks.retain(|_, t| matches!(t.status, BackgroundTaskStatus::Running));
        drop(tasks);

        // 推送 BgTaskCompleted 事件（携带完整 result 供下游注入主 agent inbox）
        self.push_event(BgRegistryEvent::Completed {
            task_id: task_id.to_string(),
            success,
            output_preview,
            duration_ms,
            result,
        });
    }

    /// 获取所有任务状态（UI 使用）
    pub fn list_tasks(&self) -> Vec<(String, BackgroundTaskStatus, String)> {
        self.tasks
            .lock()
            .values()
            .map(|t| (t.id.clone(), t.status.clone(), t.prompt_summary.clone()))
            .collect()
    }

    /// 获取完整任务信息（供 ACP Snapshot / TUI 面板使用）
    pub fn list_tasks_full(&self) -> Vec<BgTaskInfo> {
        self.tasks
            .lock()
            .values()
            .map(|t| BgTaskInfo {
                task_id: t.id.clone(),
                kind: t.kind,
                summary: t.prompt_summary.clone(),
                status: t.status.clone(),
                started_at: t.chrono_started_at.to_rfc3339(),
                duration_ms: t.started_at.elapsed().as_millis() as u64,
                pid: t.pid,
                output_preview: t.output_preview.clone(),
            })
            .collect()
    }

    /// 取消指定任务（按 BgCancelHandle 分发取消逻辑）
    pub fn cancel(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.remove(task_id) {
            match task.cancel_handle {
                BgCancelHandle::Abort(handle) => {
                    handle.abort();
                }
                BgCancelHandle::Kill(Some(tx)) => {
                    let _ = tx.send(());
                }
                BgCancelHandle::Kill(None) => {
                    warn!(
                        task_id = %task_id,
                        "bg task cancel: kill_tx already consumed"
                    );
                }
                BgCancelHandle::Pid(pid) => {
                    if pid == 0 {
                        // 防御性守卫：Pid(0) 会导致 kill -TERM 0 波及当前进程组
                        warn!(
                            task_id = %task_id,
                            "bg task cancel: pid is 0 (spawn likely failed), skipping kill"
                        );
                    } else {
                        // 通过 kill 命令发送 SIGTERM（跨平台 Unix）
                        let _ = std::process::Command::new("kill")
                            .arg("-TERM")
                            .arg(pid.to_string())
                            .spawn();
                    }
                }
            }
            drop(tasks);

            self.push_event(BgRegistryEvent::Cancelled {
                task_id: task_id.to_string(),
                reason: "user cancelled".to_string(),
            });

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

impl BackgroundTaskRegistry {
    /// 推送 registry 事件到 ACP 层（非阻塞，channel 满时静默丢弃）
    fn push_event(&self, event: BgRegistryEvent) {
        if let Some(sender) = self.event_sender.read().as_ref() {
            if sender.send(event).is_err() {
                warn!("background registry: event channel closed");
            }
        }
    }
}

#[cfg(test)]
#[path = "background_test.rs"]
mod tests;
