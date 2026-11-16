//! `/clear` 命令 — 清空对话历史。

use async_trait::async_trait;
use peri_acp_types::command::{CommandHandler, CommandOutcome};

use super::{CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel};
use crate::session::executor::PromptStopReason;

/// 清空历史命令。
#[derive(Default)]
pub struct ClearCommand;

impl ClearCommand {
    pub const NAME: &'static str = "clear";
    /// 别名（注册条目挂载，命令声明单一事实源；旧 AgentCommand impl 已删）。
    pub const ALIASES: &'static [&'static str] = &["cls", "reset"];
    /// 描述（注册条目挂载）。
    pub const DESCRIPTION: &'static str = "清空当前会话的对话历史";
}

// 新契约主实现（Phase 5 Step 3）：无参（ArgsSchema::default()，注册条目挂载）；
// 反馈经 CommandFeedback 双通道（UiOnly，不进会话）——事件发射统一收敛到编排层
// emit_command_feedback，命令内零事件代码。
#[async_trait]
impl CommandHandler for ClearCommand {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: Vec::new(), // 语义保持：清空后会话为空
            stop_reason: PromptStopReason::EndTurn,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Info,
                message: "对话已清空".to_string(),
                channel: FeedbackChannel::UiOnly,
            }),
        })
    }
}
