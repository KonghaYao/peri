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
//! - [`events`] 子模块统一 `event_sink.push_event` 模板（消除 4 处 CompactError 重复）。
//!
// [TRAP] Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。
// CompactCommand 自身不调用 push_done（由 executor.rs 的 Immediate 路径负责）。
// （详见 spec/global/domains/agent.md#issue_2026-05-29-immediate-command-missing-push-done）

mod events;
mod pipeline;

use async_trait::async_trait;

use super::{AgentCommand, CommandContext, CommandKind, CommandResult};

/// 手动 compact 命令。
pub struct CompactCommand;

impl CompactCommand {
    pub const NAME: &'static str = "compact";
}

#[async_trait]
impl AgentCommand for CompactCommand {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["compress"]
    }

    fn description(&self) -> &str {
        "压缩对话历史以释放上下文空间"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        // 纯 shim：所有业务逻辑（Pipeline 编排 + 终态映射）已迁入 pipeline.rs。
        pipeline::execute_compact(ctx).await
    }
}

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;
