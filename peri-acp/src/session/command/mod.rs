//! ACP Slash Commands — 命令基础设施。
//!
//! 定义命令 trait、注册表、执行上下文和结果类型。
//! 命令在 executor 入口拦截，`Immediate` 类型不构建 agent 直接执行。

pub mod bg;
pub mod clear;
pub mod compact;
pub mod rewind;

/// Rewind 文件复原相关符号——供 dispatch 层（`session/rewind-preview` 预算）
/// 复用 `extract_file_changes` / `FileChange`。
pub(crate) use rewind::{extract_file_changes, FileChange, RewindCommand};

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::ThreadStore;

use crate::{
    provider::PeriConfig,
    session::{event_sink::EventSink, executor::PromptStopReason},
};

/// `/bg` fork agent 启动请求（纯数据，跨层透传）。
///
/// 命令定义（`host/exec/bg.rs`）只构造本请求并交给注入的
/// [`BgForkSpawner`]；深绑 Agent 层类型（LLM 构造 / 工具集 / SubAgent
/// 发起）的实现在 executor 装配面（`host/exec/executor_helpers.rs`），
/// 命令层不引用 Agent 层业务面（3.0 批 2：装配注入）。
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
    /// 会话配置（LLM 构造用）。
    pub peri_config: Arc<crate::provider::PeriConfig>,
}

/// `/bg` fork agent 启动接口（装配注入）。
///
/// 实现方为 executor 装配面（深绑 Agent 层 `SessionFactory`，L3 迁出后经
/// 统一入口调用）；命令定义只经本接口发起，不直接引用 Agent 层类型。
#[async_trait]
pub trait BgForkSpawner: Send + Sync {
    /// 启动后台 fork agent。返回 `Err(用户可见错误信息)`。
    async fn spawn_fork(&self, req: BgForkRequest) -> Result<(), String>;
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

/// 命令执行上下文。
pub struct CommandContext {
    pub session_id: String,
    pub history: Vec<BaseMessage>,
    pub cwd: String,
    pub peri_config: Arc<PeriConfig>,
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。由 executor 从 provider 构造后传入。
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub event_sink: Arc<dyn EventSink>,
    /// 命令参数（命令名之后的文本）。
    pub args: String,
    /// 取消令牌，用于 Ctrl+C 打断长时间运行的命令（如 compact 的 LLM 调用）。
    pub cancel_token: tokio_util::sync::CancellationToken,
    /// 持久化存储，用于 rewind 等需要删除消息的命令。
    pub thread_store: Option<Arc<dyn peri_acp_types::store::ThreadStore>>,
    /// 当前会话的 thread ID，配合 thread_store 使用。
    pub thread_id: Option<String>,
    /// 后台任务事件的发送通道（BgCommand 等 Immediate 命令依赖）。
    /// Option 因为非 bg 命令路径不需要。BgCommand 总是 expect 它是 Some。
    pub bg_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<peri_acp_types::event::ExecutorEvent>>,
    /// 后台任务管理器（BgCommand 等 Immediate 命令依赖）。
    /// Option 因为非 bg 命令路径不需要。BgCommand 总是 expect 它是 Some。
    pub task_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    /// Frozen CLAUDE.md main content（会话级捕获，BgCommand 透传到 fork agent）。
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建）。
    pub frozen_system_prompt: Option<Arc<String>>,
    /// `/bg` fork agent 启动器（装配注入，3.0 批 2）。
    /// None = 未注入（RPC 直调等缺少装配面的路径），BgCommand 优雅报错。
    pub bg_spawner: Option<Arc<dyn BgForkSpawner>>,
}

/// 命令执行结果。
pub struct CommandResult {
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 停止原因。
    pub stop_reason: PromptStopReason,
}

/// Agent 侧命令 trait。
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

/// 命令注册表。
pub struct CommandRegistry {
    commands: Vec<Box<dyn AgentCommand>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn register(&mut self, cmd: Box<dyn AgentCommand>) {
        self.commands.push(cmd);
    }

    /// 按名称或别名查找命令。返回 `(命令引用, 剩余参数)`。
    pub fn find<'a>(&'a self, text: &'a str) -> Option<(&'a dyn AgentCommand, &'a str)> {
        let text = text.trim_start_matches('/');
        let (name, args) = match text.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (text.trim(), ""),
        };
        if name.is_empty() {
            return None;
        }

        // 1) 精确匹配 name
        for cmd in &self.commands {
            if cmd.name() == name {
                return Some((cmd.as_ref(), args));
            }
        }
        // 2) 前缀匹配 name（/rew → /rewind）。仅当唯一前缀时生效，多个歧义前缀退化为无匹配。
        let prefix_matches: Vec<&Box<dyn AgentCommand>> = self
            .commands
            .iter()
            .filter(|cmd| cmd.name().starts_with(name) && cmd.name() != name)
            .collect();
        if prefix_matches.len() == 1 {
            return Some((prefix_matches[0].as_ref(), args));
        }
        // 3) 精确匹配 alias
        for cmd in &self.commands {
            if cmd.aliases().contains(&name) {
                return Some((cmd.as_ref(), args));
            }
        }
        None
    }

    /// 返回所有注册命令的 `(name, description, aliases)` 元组。
    pub fn list(&self) -> Vec<(&str, &str, Vec<&str>)> {
        self.commands
            .iter()
            .map(|c| (c.name(), c.description(), c.aliases()))
            .collect()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        default_command_registry()
    }
}

/// 创建包含所有内置命令的默认注册表。
pub fn default_command_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(bg::BgCommand));
    reg.register(Box::new(compact::CompactCommand));
    reg.register(Box::new(clear::ClearCommand));
    reg.register(Box::new(rewind::RewindCommand));
    reg
}

/// 创建仅包含 agent 内部命令的注册表（供 prompt 拦截用）。
/// 视图层命令（/clear、/rewind）不在此注册表中——它们由 TUI kit 路径拦截处理。
pub fn default_prompt_command_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Box::new(compact::CompactCommand));
    reg.register(Box::new(bg::BgCommand));
    reg
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
