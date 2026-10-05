//! 后台任务契约（自 peri-agent 迁入；`peri-agent::agent::async_tasks` 保留 re-export）。
//!
//! 仅承载跨层数据契约（kind / registry 事件 / 管理接口）；
//! `TaskManager` / `BackgroundTaskRegistry` 等运行时实现留在 peri-agent
//! （per-session 聚合，生命周期/取消/事件跟随 session，§2 async tasks manager）。

use std::sync::Arc;
use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

use crate::event::BackgroundTaskResult;

/// 后台任务类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgTaskKind {
    Shell,
    Agent,
    Workflow,
    Mcp,
}

/// 后台任务注册表事件（registry → executor 事件推送通道）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
// BackgroundTaskResult is the canonical completion DTO and must remain
// non-boxed for existing event consumers; keep the enum wire-compatible.
#[allow(clippy::large_enum_variant)]
pub enum BgRegistryEvent {
    Started {
        task_id: String,
        kind: BgTaskKind,
        summary: String,
        started_at: String,
    },
    Completed {
        task_id: String,
        kind: Option<BgTaskKind>,
        success: bool,
        output_preview: String,
        duration_ms: u64,
        result: BackgroundTaskResult,
    },
    Cancelled {
        task_id: String,
        reason: String,
    },
    Updated {
        task_id: String,
        status: String,
    },
}

/// One change in a session task stream. Subscribe before reading a snapshot,
/// then discard changes at or below its revision.
#[derive(Debug, Clone)]
pub struct TaskChange {
    pub revision: u64,
    pub event: BgRegistryEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub kind: BgTaskKind,
    pub summary: String,
    pub started_at: String,
    pub status: String,
    pub duration_ms: u64,
    pub output_preview: Option<String>,
    /// 投递归属（直接发起会话）；`None` = 未记录（本地 owner 任务或测试投影）。
    /// 快照/增量事件携带它，root 面板据此归属子会话发起的任务（只读投影）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiator_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub revision: u64,
    pub tasks: Vec<TaskRecord>,
}

pub type ExternalCancelFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;
/// Terminal delivery hook. Delivery is durable and confirmed before the task
/// projection publishes a terminal state, so it cannot be a synchronous
/// fire-and-forget call: callers await the returned future and keep the task
/// unsettled when it fails.
pub type ExternalNotifyFn = Arc<
    dyn Fn(
            &BackgroundTaskResult,
            crate::messages::MessageId,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// Durable terminal-reminder route into the session that initiated a task.
///
/// The route commits the reminder into the initiator's canonical transcript
/// (idempotent by delivery ID, so a crash before commit can retry) and
/// best-effort wakes a live initiator through its own queue. MQ enqueue alone
/// is not delivery.
pub trait TaskTerminalDelivery: Send + Sync {
    fn deliver<'a>(
        &'a self,
        delivery_id: crate::messages::MessageId,
        reminder: &'a crate::system_reminder::TrustedSystemReminder,
        source: crate::session::MessageSource,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

/// Identity is supplied by the trusted connection adapter, never tool input.
pub struct ExternalTaskRegistration {
    /// Session that owns the task projection, scope and recovery reconciliation.
    pub session_id: String,
    /// Session that initiated the tool call. A known initiator is immutable;
    /// discovery without one cannot replace its route. An unknown initiator
    /// is unroutable and never implies delivery to a parent or root session.
    pub initiator_session_id: Option<String>,
    pub owner_identity: String,
    pub owner_task_id: String,
    pub kind: BgTaskKind,
    pub summary: String,
    /// Owner creation timestamp (RFC3339); absent only when the owner omits it.
    pub started_at: Option<String>,
    pub cancel: ExternalCancelFn,
    pub on_terminal: ExternalNotifyFn,
}

/// 后台任务注册请求（middleware / workflow 发起面 → `TaskManager::register` 的
/// 输入契约；具体任务簿记字段——agent_name / status / cancel_handle——由实现方
/// 按 kind 补全，发起方不触碰实现细节）。
pub struct BgTaskRegistration {
    /// 任务标识（uuid7）。
    pub task_id: String,
    /// 任务类别（Shell / Workflow 按 kind 独立并发上限；Agent 类不限额）。
    pub kind: BgTaskKind,
    /// 任务摘要（prompt_summary / 命令摘要）。
    pub summary: String,
    /// OS 进程 PID（bg shell 有效；None = 无进程句柄）。
    pub pid: Option<u32>,
    /// kill 闭包（Workflow 类任务的取消转发；None = kill 通道不可用）。
    pub kill: Option<Box<dyn FnOnce() + Send + Sync>>,
}

/// 确认终态已发布到接收方；失败或 panic 时 owner 保留原结果，不能提前完成任务。
/// `Ok(())` 确认接纳，不表示 Receive 或模型已经处理。
pub type OnBgCompleteFn =
    Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) -> Result<(), String> + Send + Sync>;

/// Cleanup evidence for a session's background execution scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskShutdownReport {
    Complete,
    Incomplete,
}

pub type OwnedTaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Keeps external execution in the session scope until its caller proves cleanup.
/// Dropping without confirmation leaves shutdown incomplete.
pub trait ExternalExecutionGuard: Send {
    fn confirm_stopped(&mut self);
}

/// 后台 shell 启动结果（`TaskManager::spawn_shell` 返回值）。
///
/// 这是执行环境侧的产品：pid 与输出日志路径供宿主/面板与本地执行环境使用。
/// 经 MCP Tasks 暴露给模型时，模型面回执只携带 opaque task id（`mcp-` 前缀，
/// 见 `McpToolBridge`）；完成提醒携带退出信息与输出文件引用，不承诺 pid。
#[derive(Debug, Clone)]
pub struct BgShellHandle {
    /// 任务标识（`shell-{uuid v7}`）。
    pub task_id: String,
    /// OS 进程 PID（Unix 下为进程组组长：`kill -- -{pid}` 可杀整组含子进程，
    /// 本地执行环境取消会先 TERM，再有界等待并升级 KILL）。
    /// `None` = 进程 spawn 失败（任务注册后立即按失败收尾，失败通知仍会到达）。
    pub pid: Option<u32>,
    /// stdout 实时输出日志文件路径（运行期间持续追加，agent 可用 Read 读取；
    /// 完成后文件保留）。`None` = 日志不可用（spawn 失败或文件创建失败）。
    pub stdout_log: Option<String>,
    /// stderr 实时输出日志文件路径（同上）。
    pub stderr_log: Option<String>,
}

/// 后台任务管理接口（跨层面：ACP session 生命周期、/bg 并发预检、
/// middleware 的 shell 发起与完成收尾使用）。
///
/// 生命周期实现（registry 簿记、准入、完成与关闭证据）留在 peri-agent
/// `TaskManager`；shell 执行与输出由注入的执行环境承担。本 trait 只承载跨层操作，
/// `Arc<dyn TaskManager>` 由 Agent 层实现、经装配注入到 ACP / middlewares。
pub trait TaskManager: std::any::Any + Send + Sync {
    fn restore_external_terminal(
        &self,
        _request: ExternalTaskRegistration,
        _terminal_transition_id: &str,
        _result: BackgroundTaskResult,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        Box::pin(async { Err("external terminal restoration is unavailable".into()) })
    }
    fn external_task_ids(&self) -> Vec<String> {
        Vec::new()
    }
    fn has_unsettled_external(&self) -> bool {
        false
    }
    fn mark_external_lost(&self, _task_id: &str) -> bool {
        false
    }
    fn mark_external_running(&self, _task_id: &str) -> bool {
        false
    }
    fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            revision: 0,
            tasks: Vec::new(),
        }
    }
    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TaskChange> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
    fn register_external(&self, _request: ExternalTaskRegistration) -> Result<String, String> {
        Err("external tasks are unavailable".into())
    }
    fn settle_external(
        &self,
        _task_id: &str,
        _terminal_transition_id: &str,
        _result: BackgroundTaskResult,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        Box::pin(async { Err("external terminal delivery is unavailable".into()) })
    }

    /// Terminal give-up for an external task whose owner stays unobservable for
    /// the whole bounded retry window. Delivers one final, explicitly marked
    /// reminder and settles the projection, so a lost task can neither stay
    /// active forever nor disappear silently. Returns whether a terminal record
    /// was published.
    fn abandon_external(
        &self,
        _task_id: &str,
        _reason: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }
    fn cancel_async(
        &self,
        task_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let task_id = task_id.to_owned();
        Box::pin(async move { self.cancel(&task_id) })
    }
    /// Record actual external drain without changing notification delivery or UI state.
    /// Call only after the registered execution and its children have joined.
    fn confirm_external_execution_stopped(&self, _task_id: &str) {}
    /// No owned execution or unresolved external cleanup remains. UI task count is insufficient.
    fn is_execution_idle(&self) -> bool {
        false
    }
    /// 向下转型（装配面需要具体类型时用，如 /bg 的 SubAgent 发起）。
    fn as_any(&self) -> &dyn std::any::Any;

    /// 转为 `Arc<dyn Any + Send + Sync>`（供 `Arc::downcast` 还原具体类型）。
    fn as_arc_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>
    where
        Self: Sized,
    {
        self
    }
    /// 事件桥接（过渡态）：注入 BgRegistryEvent 推送通道（ACP executor 的
    /// registry 事件泵消费；随 M-event-chain 归一收口）。
    fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    );

    /// 当前活跃任务数（/bg 并发限制预检）。
    fn active_count(&self) -> usize;

    /// 按类型注册任务（Shell / Workflow 有 kind 独立并发上限；Agent 类不限额；
    /// middleware 发起面调用，错误语义经 String 表达——并发上限 / 注册失败）。
    fn register(&self, request: BgTaskRegistration) -> Result<(), String>;

    /// 仅结算无需通知接收方的 execution-only 任务，或已确认交付的外部任务。
    /// 需要通知接收方的 owned 任务必须使用 settle_completed。
    fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool;

    /// 需要通知接收方的 owned 任务统一由 owner 执行交付后结算。
    /// 交付失败保持 delivery_pending；重复已完成结算返回 false。
    fn settle_completed(
        &self,
        task_id: &str,
        result: BackgroundTaskResult,
        delivery: OnBgCompleteFn,
    ) -> Result<bool, String>;

    /// 重试保留的结果，返回已成功交付的数量，不重跑任务执行。
    fn retry_pending_deliveries(&self) -> usize;

    /// 取消任务（ACP session/cancel_task 定位转发；错误语义经 String 表达，
    /// ACP 侧包 context 为协议错误）。
    fn cancel(&self, task_id: &str) -> Result<(), String>;

    /// 取消全部 owned 任务（session 销毁 / close_session 时调用）。
    fn cancel_all(&self);

    /// Signals session shutdown to owned work without a user-visible task entry.
    /// Cancellation requests cleanup; `spawn_owned` still tracks its completion.
    fn execution_cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        None
    }

    /// Spawn within the session's tracked scope; reject after shutdown starts.
    fn spawn_owned(&self, _task: OwnedTaskFuture) -> Result<tokio::task::JoinHandle<()>, String> {
        Err("task manager does not support owned execution".into())
    }

    /// Keep an external call inside this session's execution evidence until the
    /// caller proves cleanup. `scope` identifies the external owner (for
    /// example the MCP server name, or `workspace` for injected shell
    /// execution) so a conclusive reconciliation of that owner can clear only
    /// its own uncertainty.
    fn begin_external_execution(
        &self,
        _scope: &str,
    ) -> Result<Box<dyn ExternalExecutionGuard>, String> {
        Err("task manager does not support external execution ownership".into())
    }

    /// Clear uncertainty recorded for `scope` after a conclusive reconciliation
    /// (for example a fully applied task snapshot for that owner). Returns how
    /// many records were cleared. Callers must hold real evidence; without it
    /// the scope must stay non-idle.
    fn resolve_external_execution_evidence(&self, _scope: &str) -> usize {
        0
    }

    /// Stop admission and await actual cleanup. A cancellation request is not completion.
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = TaskShutdownReport> + Send + '_>> {
        self.cancel_all();
        Box::pin(async { TaskShutdownReport::Incomplete })
    }

    /// 启动后台 shell 任务（run_in_background 路径；Agent 管理准入和生命周期，
    /// 注入的执行环境管理进程 spawn / 进程组 / 超时 / 输出收集）。
    ///
    /// 返回 [`BgShellHandle`]（task_id + 进程 PID）：工具层回显给 LLM，
    /// 使 LLM 能经另一个 shell 杀进程组（`kill -- -{pid}`）或凭 task_id 监控。
    fn spawn_shell(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>>;

    /// 后台 shell 完成收尾：接收已写入的输出文件引用，认领完成后通知并提交终态。
    #[allow(clippy::too_many_arguments)] // 收尾参数集为跨层固定契约，不分组
    fn finalize_bg_shell(
        &self,
        on_bg_complete: &Option<OnBgCompleteFn>,
        task_id: String,
        prompt_summary: String,
        success: bool,
        output: String,
        duration_ms: u64,
        timed_out: bool,
        shell_output: Option<crate::event::ShellOutput>,
    );
}

/// 空实现（fallback：session 未注入 TaskManager 时——print 模式等无 bg 场景）。
pub struct NoopTaskManager;

impl TaskManager for NoopTaskManager {
    fn is_execution_idle(&self) -> bool {
        true
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_event_sender(
        &self,
        _sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        _session_id: String,
    ) {
    }

    fn active_count(&self) -> usize {
        0
    }

    fn register(&self, _request: BgTaskRegistration) -> Result<(), String> {
        Err("no task manager configured".to_string())
    }

    fn complete(&self, _task_id: &str, _result: BackgroundTaskResult) -> bool {
        false
    }

    fn settle_completed(
        &self,
        _task_id: &str,
        _result: BackgroundTaskResult,
        _delivery: OnBgCompleteFn,
    ) -> Result<bool, String> {
        Err("task completion delivery is unavailable".into())
    }

    fn retry_pending_deliveries(&self) -> usize {
        0
    }

    fn cancel(&self, _task_id: &str) -> Result<(), String> {
        Err("no task manager configured".to_string())
    }

    fn cancel_all(&self) {}

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = TaskShutdownReport> + Send + '_>> {
        Box::pin(async { TaskShutdownReport::Complete })
    }

    fn spawn_shell(
        &self,
        _command: String,
        _cwd: String,
        _timeout_ms: Option<u64>,
        _on_bg_complete: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        Err("no task manager configured".into())
    }

    fn finalize_bg_shell(
        &self,
        _on_bg_complete: &Option<OnBgCompleteFn>,
        _task_id: String,
        _prompt_summary: String,
        _success: bool,
        _output: String,
        _duration_ms: u64,
        _timed_out: bool,
        _shell_output: Option<crate::event::ShellOutput>,
    ) {
    }
}
