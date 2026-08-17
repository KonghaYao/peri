use std::collections::HashMap;

use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::events::BackgroundTaskResult;

use super::shell::kill_process_group;

/// bg agent 取消的优雅退出窗口（秒）：cancel() 先 `token.cancel()` 让任务响应
/// 取消链走完整收尾；超过该窗口任务仍未结束才 abort 兜底。
const CANCEL_GRACE_SECS: u64 = 3;

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
    #[error("Task {0} cannot be cancelled: kill handle unavailable")]
    KillUnavailable(String),
    #[error("Kind concurrent limit reached: {kind} ({current}/{limit})")]
    KindConcurrentLimit {
        kind: String,
        current: usize,
        limit: usize,
    },
}

pub enum BgCancelHandle {
    /// bg agent：取消 tokio task。
    /// 持 `JoinHandle`（而非 `AbortHandle`）——取消时先 `token.cancel()` 让任务
    /// 优雅退出，再 await JoinHandle 等待其走完收尾，超时才 abort。
    Abort(tokio::task::JoinHandle<()>),
    /// workflow：kill 闭包——转发到 `WorkflowTaskRegistry::kill`（真正的 kill_tx 在其内部）。
    /// `None` 表示 kill 通道不可用（如 spawn 失败），此时 `cancel()` 返回明确错误
    /// 而非假装成功（issue 2026-08-05：Workflow 取消无效）。
    Kill(Option<Box<dyn FnOnce() + Send + Sync>>),
    /// bg shell：OS 进程 kill
    Pid(u32),
}

impl std::fmt::Debug for BgCancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BgCancelHandle::Abort(_) => f.write_str("Abort(_)"),
            BgCancelHandle::Kill(_) => f.write_str("Kill(_)"),
            BgCancelHandle::Pid(pid) => f.debug_tuple("Pid").field(pid).finish(),
        }
    }
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
    /// 取消令牌（仅 Agent 类任务）：cancel() 时先 `token.cancel()` 让工具层取消链
    /// 生效（run_react_loop 的 await 点响应后走完整收尾），超时再 abort 兜底。
    /// Shell/Workflow 类任务为 None（取消走 Pid/Kill 句柄）。
    pub cancel_token: Option<CancellationToken>,
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

/// 后台任务注册中心
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
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

    /// 任务完成时调用：更新状态 + 推送通知。
    ///
    /// 返回 `true` 表示条目存在且已处理；`false` 表示任务已不在 registry
    /// （如已被 cancel 移除后自然完成），此时不推送 Completed 事件——否则会
    /// 产生幽灵完成事件（issue 2026-08-05：kill 后仍推 bg-task-completed）。
    pub fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool {
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
        let kind = tasks.get(task_id).map(|task| task.kind);
        let existed = tasks.contains_key(task_id);
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

        // 已移除条目不推幽灵 Completed 事件（cancel 已通知过用户）。
        // warn 而非静默：任务不在 registry 却走到 complete()，通常是
        // task_id 碰撞覆盖注册（同毫秒 UUID v7 截断前缀）或双重 complete，
        // 会导致 TUI 任务条目残留（issue 2026-08-05）。
        if !existed {
            warn!(
                task_id = %task_id,
                agent_name = %result.agent_name,
                success,
                "background registry: complete() called for unknown task (collision or double-complete); \
                 Completed event suppressed"
            );
            return false;
        }

        // 推送 BgTaskCompleted 事件（携带完整 result 供下游注入主 agent inbox）
        self.push_event(BgRegistryEvent::Completed {
            task_id: task_id.to_string(),
            kind,
            success,
            output_preview,
            duration_ms,
            result,
        });
        true
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
        // 先校验取消句柄可用性：Kill(None) 表示 kill 通道不可用（如 workflow kill 闭包缺失、
        // shell spawn 失败），此时如实返回错误并保留条目，等待任务自然完成，
        // 而不是移除条目 + 发 cancelled 事件假装成功（issue 2026-08-05）。
        let handle_unavailable = matches!(
            tasks.get(task_id).map(|t| &t.cancel_handle),
            Some(BgCancelHandle::Kill(None))
        );
        if handle_unavailable {
            return Err(BackgroundRegistryError::KillUnavailable(
                task_id.to_string(),
            ));
        }
        if let Some(task) = tasks.remove(task_id) {
            match task.cancel_handle {
                BgCancelHandle::Abort(mut handle) => {
                    // S3.2：先触发工具层取消链——任务在下一个响应 cancel 的 await 点
                    // （reason LLM 调用 / 工具执行 / idle 等待）退出，走完整收尾
                    // （SubagentStopped / deregister / thread status / stop hooks）。
                    if let Some(token) = task.cancel_token.as_ref() {
                        token.cancel();
                    }
                    // 超时兜底：等待任务自然结束（grace 窗口内响应 cancel 则保留
                    // async 收尾），超时再 abort——否则"取消后任务继续跑"比 abort 更糟。
                    // abort 兜底路径：任务内同步收尾 guard（deregister_runtime 等）仍执行，
                    // async 收尾（update_thread_status / stop hooks）丢失并记日志。
                    match tokio::runtime::Handle::try_current() {
                        Ok(_) => {
                            let task_id_owned = task_id.to_string();
                            tokio::spawn(async move {
                                if tokio::time::timeout(
                                    std::time::Duration::from_secs(CANCEL_GRACE_SECS),
                                    &mut handle,
                                )
                                .await
                                .is_err()
                                {
                                    handle.abort();
                                    warn!(
                                        task_id = %task_id_owned,
                                        "bg task cancel: grace period elapsed, aborted task \
                                         (async cleanup lost: thread status / stop hooks; \
                                         sync cleanup guard still runs)"
                                    );
                                }
                            });
                        }
                        Err(_) => {
                            // 无 tokio runtime 上下文（防御；生产调用点均在 async 上下文）：
                            // 无法异步等待，直接 abort 兜底。
                            handle.abort();
                            warn!(
                                task_id = %task_id,
                                "bg task cancel: no tokio runtime for graceful wait, aborted task"
                            );
                        }
                    }
                }
                BgCancelHandle::Kill(Some(kill)) => {
                    // 触发 kill 闭包：workflow 场景转发到 WorkflowTaskRegistry::kill
                    kill();
                }
                BgCancelHandle::Kill(None) => {
                    // 上方已校验，理论不可达；防御性保留
                    unreachable!("Kill(None) checked before task removal");
                }
                BgCancelHandle::Pid(pid) => {
                    if pid == 0 {
                        // 防御性守卫：Pid(0) 会导致 kill -TERM 0 波及当前进程组
                        warn!(
                            task_id = %task_id,
                            "bg task cancel: pid is 0 (spawn likely failed), skipping kill"
                        );
                    } else {
                        // 杀整个进程组（bash 为组长），避免子进程孤儿存活
                        kill_process_group(pid, "TERM");
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
