//! `/compact` 命令 — 手动触发上下文压缩。
//!
//! 移植自 `peri-tui/src/acp_server/compact.rs`，
//! 改为接收 [`CommandContext`]、返回 [`CommandResult`]。
//!
//! ## 模块组织（Facade + Module-per-Feature）
//!
//! - [`CompactCommand`] 是对外 public 类型，仅做 Pipeline 编排（Orchestration）。
//! - [`pipeline`] 子模块实现各阶段：validate → resolve_model → run_full_compact
//!   → re_inject → assemble_messages，每阶段一个纯函数 + 显式输入输出类型。
//!
//! [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
//! CompactCommand 自身不调用 push_done（由 executor.rs 的 Immediate 路径负责）。
//! （详见 spec/global/domains/agent.md#issue_2026-05-29-immediate-command-missing-push-done）

pub(crate) mod events;
mod pipeline;

use async_trait::async_trait;
use peri_acp_types::command::{CommandHandler, CommandOutcome};

use super::CommandContext;

/// 手动 compact 命令。
#[derive(Default)]
pub struct CompactCommand;

impl CompactCommand {
    pub const NAME: &'static str = "compact";
    /// 别名（注册条目挂载，命令声明单一事实源；旧 AgentCommand impl 已删）。
    pub const ALIASES: &'static [&'static str] = &["compress"];
    /// 描述（注册条目挂载）。
    pub const DESCRIPTION: &'static str = "压缩对话历史以释放上下文空间";
}

// 新契约主实现（Phase 5 Step 4）：无参命令（ArgsSchema::default()，注册条目
// 挂载）；执行体在 agent 层 compact_pipeline（`pipeline::execute_compact`）；
// 反馈经 CommandFeedback 双通道（UiOnly，不进会话）——事件发射统一收敛到
// 编排层 emit_command_feedback，命令内零事件代码（CompactStarted /
// CompactCompleted 阶段信号由 pipeline 保留）。
#[async_trait]
impl CommandHandler for CompactCommand {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(pipeline::execute_compact(ctx).await)
    }
}

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;
