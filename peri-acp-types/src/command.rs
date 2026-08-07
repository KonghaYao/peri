//! Slash 命令契约（L5：命令执行体迁入 Agent 层的边界端口）。
//!
//! 自 `peri-acp/src/session/command/mod.rs` 与 `peri-acp/src/host/exec/`
//! 命令执行体（`bg` / `compact_pipeline`）迁入：命令定义（[`AgentCommand`]）、
//! 执行上下文（[`CommandContext`]）、/bg fork 请求（[`BgForkRequest`] +
//! [`BgForkSpawner`]）与命令/执行终态（[`CommandResult`] / [`PromptStopReason`]）
//! 归本层，Agent 层命令实现经本契约执行，ACP 保留协议化薄壳与装配面
//! （命令注册表 / spawner 实现 / EventSink 实现）。
//!
//! 依赖反转说明：
//! - `peri_config`（ACP provider 配置）不进入本契约——`CommandContext` 以
//!   `compact_config`（compact 管线输入）投影，/bg fork 的 LLM 构造由
//!   [`BgForkSpawner`] 实现方（ACP 装配面）自持配置；
//! - 事件发射经 [`crate::event::EventSink`] 端口（ACP 实现，协议序列化面）。

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::compact::CompactConfig;
use crate::event::{EventSink, ExecutorEvent};
use crate::messages::BaseMessage;
use crate::store::ThreadStore;
use crate::tasks::TaskManager;

/// 命令执行停止原因（`executor::PromptStopReason` 契约化，ACP re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    /// 正常完成——agent 完成本轮。
    EndTurn,
    /// 用户经 `session/cancel` 取消。
    Cancelled,
    /// agent 达到最大迭代次数。
    MaxTurnRequests,
}

/// 命令执行方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// 直接执行，不构建 agent（如 compact、clear）。
    Immediate,
    /// 透传到正常 agent 管线（如 skills）。
    Passthrough,
    /// [预留] 变换 prompt 内容后传给 agent。
    Transform,
}

/// 命令执行上下文（L5 契约化：原 ACP `session::command::CommandContext`）。
///
/// `peri_config`（ACP provider 配置）不进入本结构：
/// - compact 管线使用 [`CommandContext::compact_config`]（ACP 装配点按
///   `load_compact_config` 语义预填，env overrides 每轮重新应用）；
/// - /bg fork 的 LLM 构造由 [`BgForkSpawner`] 实现方自持配置。
pub struct CommandContext {
    pub session_id: String,
    pub history: Vec<BaseMessage>,
    pub cwd: String,
    /// compact 管线配置（ACP 装配点按 `load_compact_config` 语义预填：
    /// unwrap_or_default + env overrides，每次 CommandContext 构造 = 每轮）。
    pub compact_config: CompactConfig,
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub event_sink: Arc<dyn EventSink>,
    /// 命令参数（命令名之后的文本）。
    pub args: String,
    /// 取消令牌，用于 Ctrl+C 打断长时间运行的命令（如 compact 的 LLM 调用）。
    pub cancel_token: CancellationToken,
    /// 持久化存储，用于 rewind 等需要删除消息的命令。
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 当前会话的 thread ID，配合 thread_store 使用。
    pub thread_id: Option<String>,
    /// 后台任务事件的发送通道（BgCommand 等 Immediate 命令依赖）。
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（BgCommand 等 Immediate 命令依赖）。
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Frozen CLAUDE.md main content（会话级捕获，BgCommand 透传到 fork agent）。
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建）。
    pub frozen_system_prompt: Option<Arc<String>>,
    /// `/bg` fork agent 启动器（装配注入）。None = 未注入（RPC 直调等缺少
    /// 装配面的路径），BgCommand 优雅报错。
    pub bg_spawner: Option<Arc<dyn BgForkSpawner>>,
}

/// 命令执行结果。
pub struct CommandResult {
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 停止原因。
    pub stop_reason: PromptStopReason,
}

/// Agent 侧命令 trait（L5：命令实现迁入 Agent 层，经本契约执行）。
#[async_trait]
pub trait AgentCommand: Send + Sync {
    /// 命令名（不含 `/` 前缀）。
    fn name(&self) -> &str;
    /// 别名列表。
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    /// 命令描述。
    fn description(&self) -> &str;
    /// 命令类型。
    fn kind(&self) -> CommandKind;
    /// 执行命令。
    async fn execute(&self, ctx: CommandContext) -> CommandResult;
}

/// `/bg` fork agent 启动请求（纯数据，跨层透传）。
///
/// 命令定义（Agent 层 `session::exec::bg::BgCommand`）只构造本请求并交给
/// 注入的 [`BgForkSpawner`]；深绑 ACP/Agent 层类型（LLM 构造 / 工具集 /
/// SubAgent 发起）的实现在装配面（`peri-agent::session::exec::executor_helpers`
/// 的 `DefaultBgForkSpawner`，经 `BgForkSpawner` 端口注入），命令层不引用
/// 业务面实现。
pub struct BgForkRequest {
    /// 后台任务描述。
    pub prompt: String,
    /// 父会话消息历史（fork 上下文）。
    pub parent_messages: Vec<BaseMessage>,
    /// 父会话 thread id。
    pub parent_thread_id: Option<String>,
    /// 工作目录。
    pub cwd: String,
    /// 冻结 CLAUDE.md main content。
    pub frozen_claude_md: Option<String>,
    /// 冻结 CLAUDE.local.md content。
    pub frozen_claude_local_md: Option<String>,
    /// 冻结 skills summary。
    pub frozen_skill_summary: Option<String>,
    /// 冻结 system prompt（fork 路径复用，避免重建）。
    pub frozen_system_prompt: Option<String>,
    /// 后台任务事件通道（子 agent 事件经此到达事件泵）。
    pub bg_event_sender: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    /// 持久化存储。
    pub thread_store: Arc<dyn ThreadStore>,
}

/// `/bg` fork agent 启动接口（装配注入）。
///
/// 实现方为 ACP executor 装配面（深绑 Agent 层 `SessionFactory`）；命令定义
/// 只经本接口发起，不直接引用 Agent 层类型。`peri_config`（LLM 构造输入）
/// 由实现方自持，不进入请求契约。
#[async_trait]
pub trait BgForkSpawner: Send + Sync {
    /// 启动后台 fork agent。返回 `Err(用户可见错误信息)`。
    async fn spawn_fork(&self, req: BgForkRequest) -> Result<(), String>;
}
