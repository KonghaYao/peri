//! ACP Slash Commands — 命令基础设施。
//!
//! 定义命令 trait、注册表、执行上下文和结果类型。
//! 命令在 executor 入口拦截，`Immediate` 类型不构建 agent 直接执行。

pub mod bg;
pub mod clear;
pub mod compact;
pub mod rewind;

use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::{llm::BaseModel, messages::BaseMessage};

use crate::{
    provider::PeriConfig,
    session::{event_sink::EventSink, executor::PromptStopReason},
};

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
    /// 用于 compact 等需要 LLM 调用的命令。由 executor 从 provider 构造后传入。
    pub compact_model: Option<Arc<dyn BaseModel>>,
    pub event_sink: Arc<dyn EventSink>,
    /// 命令参数（命令名之后的文本）。
    pub args: String,
    /// 取消令牌，用于 Ctrl+C 打断长时间运行的命令（如 compact 的 LLM 调用）。
    pub cancel_token: AgentCancellationToken,
    /// 持久化存储，用于 rewind 等需要删除消息的命令。
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// 当前会话的 thread ID，配合 thread_store 使用。
    pub thread_id: Option<String>,
    /// 后台任务事件的发送通道（BgCommand 等 Immediate 命令依赖）。
    /// Option 因为非 bg 命令路径不需要。BgCommand 总是 expect 它是 Some。
    pub bg_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>>,
    /// 后台任务注册中心（BgCommand 等 Immediate 命令依赖）。
    /// Option 因为非 bg 命令路径不需要。BgCommand 总是 expect 它是 Some。
    pub bg_registry: Option<Arc<peri_middlewares::subagent::BackgroundTaskRegistry>>,
    /// Frozen CLAUDE.md main content（会话级捕获，BgCommand 透传到 fork agent）。
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
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

        // 精确匹配 name
        for cmd in &self.commands {
            if cmd.name() == name {
                return Some((cmd.as_ref(), args));
            }
        }
        // 精确匹配 alias
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
