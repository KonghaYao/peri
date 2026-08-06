//! `/clear` 命令 — 清空对话历史。

use peri_acp_types::event::{CompactStrategy, CompactTrigger, ExecutorEvent};

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
                    token_before: 0,
                    token_after: 0,
                    strategy: CompactStrategy::Smart,
                    affected_count: 0,
                    estimated_tokens_saved: 0,
                    estimated_tokens_before: 0,
                    estimated_tokens_after: 0,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: 0,
                    full_escalation_reason: None,
                    cache_hit_rate_before: 0.0,
                    trigger: CompactTrigger::Manual,
                    outcome: peri_acp_types::compact::CompactOutcome::Skipped,
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
