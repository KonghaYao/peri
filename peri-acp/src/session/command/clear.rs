//! `/clear` 命令 — 清空对话历史。

use peri_agent::agent::events::ExecutorEvent;

use super::{AgentCommand, CommandContext, CommandKind, CommandResult};
use crate::session::executor::PromptStopReason;

/// 清空历史命令。
pub struct ClearCommand;

impl ClearCommand {
    pub const NAME: &'static str = "clear";
}

#[async_trait::async_trait]
impl AgentCommand for ClearCommand {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["cls", "reset"]
    }

    fn description(&self) -> &str {
        "清空当前会话的对话历史"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        // 发送 CompactCompleted（空 messages）复用 TUI 的 compact 清理路径：
        // pipeline.clear() → restore_completed(vec![]) → RebuildAll { prefix_len: 0 }
        // 这确保 TUI 的 view_messages 和 origin_messages 被正确清空。
        ctx.event_sink
            .push_event(
                &ctx.session_id,
                &ExecutorEvent::CompactCompleted {
                    summary: "对话已清空".to_string(),
                    files: vec![],
                    skills: vec![],
                    micro_cleared: 0,
                    messages: vec![],
                },
                0,
            )
            .await;

        CommandResult {
            messages: Vec::new(),
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}
