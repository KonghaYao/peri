//! 命令执行模型契约（Phase 1：目标执行 trait 与结果三态）。
//!
//! `CommandHandler` 是命令执行唯一模型（Phase 5 Step 6 旧 `AgentCommand`
//! trait 已整体删除）：路由裁决后的目标执行模型（设计 §71），
//! `CommandOutcome` 描述执行结果的去向——完成反馈 / 注入 agent 管线 /
//! 转发其他执行者。执行域是元数据而非类型：新增执行域 = 新增 trait
//! 实现，注册表 / 协议 / 路由核心零改动。

use async_trait::async_trait;

use crate::command::{CommandContext, CommandResult};

/// 执行结果三态（设计 §71：Done=完成并反馈，Inject=透传指令进 agent 管线，
/// Delegate=转发其他执行者：ui 域回 TUI / 未来 MCP 直连）。
pub enum CommandOutcome {
    /// 命令已完成，携带执行结果（消息历史 + 停止原因 + 可选反馈）。
    Done(CommandResult),
    /// 透传指令进正常 agent 管线（如 skill 调用）。
    Inject(String),
    /// 转发其他执行者（ui 域回 TUI 本地执行 / 未来 MCP 直连）。
    Delegate(String),
}

/// 目标执行模型（设计 §71：执行域是元数据而非类型；新增执行域 =
/// 新增 trait 实现，注册表 / 协议 / 路由核心零改动）。
///
/// 命令执行唯一模型（Phase 5 Step 6 旧 `AgentCommand` trait 已删），
/// 所有内置命令与插件命令均直接实现本 trait。
#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// ctx 为既有 [`CommandContext`]（17 字段，接口不变）。
    /// 接口注册表约定（设计 §74 不变式 5）：core 字段常驻，
    /// 扩展依赖经 `ctx.dep::<dyn Trait>()` 按接口取——拆层由
    /// Phase 2 Step 5.5 承接，本阶段仅锁定
    /// "handler 的唯一上下文接口 = CommandContext" 这一契约。
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome;
}

#[cfg(test)]
#[path = "command_handler_test.rs"]
mod tests;
