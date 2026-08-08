//! 后台任务契约（自 peri-agent 迁入；`peri-agent::agent::async_tasks` 保留 re-export）。
//!
//! 仅承载跨层数据契约（kind / registry 事件 / 管理接口）；
//! `TaskManager` / `BackgroundTaskRegistry` 等运行时实现留在 peri-agent
//! （per-session 聚合，生命周期/取消/事件跟随 session，§2 async tasks manager）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::event::BackgroundTaskResult;

/// 后台任务类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BgTaskKind {
    Shell,
    Agent,
    Workflow,
}

/// 后台任务注册表事件（registry → executor 事件推送通道）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
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
}

/// 后台任务注册请求（middleware / workflow 发起面 → `TaskManager::register` 的
/// 输入契约；具体任务簿记字段——agent_name / status / cancel_handle——由实现方
/// 按 kind 补全，发起方不触碰实现细节）。
pub struct BgTaskRegistration {
    /// 任务标识（uuid7）。
    pub task_id: String,
    /// 任务类别（按 kind 独立并发上限）。
    pub kind: BgTaskKind,
    /// 任务摘要（prompt_summary / 命令摘要）。
    pub summary: String,
    /// OS 进程 PID（bg shell 有效；None = 无进程句柄）。
    pub pid: Option<u32>,
    /// kill 闭包（Workflow 类任务的取消转发；None = kill 通道不可用）。
    pub kill: Option<Box<dyn FnOnce() + Send + Sync>>,
}

/// bg 完成回调（TaskManager 完成收尾时通知调用方）。
pub type OnBgCompleteFn = Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>;

/// 后台 shell 启动结果（`TaskManager::spawn_shell` 返回值）。
///
/// 工具层将 task_id / pid / 日志路径回显给 LLM：LLM 可通过另一个 shell
/// 执行 `kill <pid>` 终止任务，凭 task_id 在 Tasks 面板监控状态与输出预览，
/// 或经 Read 工具实时读取输出日志文件。
#[derive(Debug, Clone)]
pub struct BgShellHandle {
    /// 任务标识（`shell-{uuid v7}`）。
    pub task_id: String,
    /// OS 进程 PID（Unix 下为进程组组长：`kill -- -{pid}` 可杀整组含子进程，
    /// 与 Agent 层 `kill_process_group_escalating` 语义一致）。
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
/// 实现与完整方法面（registry 簿记、进程 spawn 等）留在 peri-agent
/// `TaskManager`（per-session 聚合根）；本 trait 只承载跨层需要的操作，
/// `Arc<dyn TaskManager>` 由 Agent 层实现、经装配注入到 ACP / middlewares。
pub trait TaskManager: std::any::Any + Send + Sync {
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

    /// 按类型注册任务（kind 独立并发上限；middleware 发起面调用，
    /// 错误语义经 String 表达——并发上限 / 注册失败）。
    fn register(&self, request: BgTaskRegistration) -> Result<(), String>;

    /// 标记任务完成（result 注入事件载荷）。
    fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool;

    /// 取消任务（ACP session/cancel_task 定位转发；错误语义经 String 表达，
    /// ACP 侧包 context 为协议错误）。
    fn cancel(&self, task_id: &str) -> Result<(), String>;

    /// 取消全部 owned 任务（session 销毁 / close_session 时调用）。
    fn cancel_all(&self);

    /// 启动后台 shell 任务（run_in_background 路径；进程 spawn / 进程组 /
    /// 超时 / 输出收集 / 完成收尾全部在 Agent 层完成）。
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

    /// 后台 shell 完成收尾（超长输出落盘 → on_bg_complete 回调 → complete）。
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
    );
}

/// 空实现（fallback：session 未注入 TaskManager 时——print 模式等无 bg 场景）。
pub struct NoopTaskManager;

impl TaskManager for NoopTaskManager {
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

    fn cancel(&self, _task_id: &str) -> Result<(), String> {
        Err("no task manager configured".to_string())
    }

    fn cancel_all(&self) {}

    fn spawn_shell(
        &self,
        _command: String,
        _cwd: String,
        _timeout_ms: Option<u64>,
        _on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        Err("no task manager configured".into())
    }

    fn finalize_bg_shell(
        &self,
        _on_bg_complete: &Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
        _task_id: String,
        _prompt_summary: String,
        _success: bool,
        _output: String,
        _duration_ms: u64,
        _timed_out: bool,
    ) {
    }
}
