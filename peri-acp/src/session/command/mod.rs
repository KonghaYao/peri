//! ACP Slash Commands — 命令基础设施。
//!
//! 命令契约已迁入 `peri-acp-types::command`，compact 执行体迁入 Agent 层；本模块保留组合根：注册表本体在契约层
//! （[`CommandRegistry`]，扁平 HashMap + alias 索引，设计 §63），本模块只做
//! 内置命令装配（[`register_builtins`]）——旧契约（[`AgentCommand`]）已随
//! Phase 5 Step 6 整体删除，命令全部以 [`CommandHandler`] 直接注册。
//!
//! 执行方式由 [`CommandOutcome`] 承载：内置命令走 `Done`；`Inject` /
//! `Delegate` 为 Phase 5/6 保留。命令在 executor 入口拦截
//! （`peri-agent::session::exec::executor_helpers::intercept_immediate_command`），
//! 命中即执行，未命中 fall-through 进 agent 管线。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource,
};
use peri_acp_types::command::{ArgsSchema, CommandHandler, CommandOutcome};

pub mod clear;
pub mod compact;
pub mod rewind;

/// Rewind 文件复原相关符号——供 dispatch 层（`session/rewind-preview` 预算）
/// 复用 `extract_file_changes` / `FileChange`；`execute_rewind` 为 slash 与
/// RPC 双入口共享执行体（Phase 5 Step 5）。
pub(crate) use rewind::{execute_rewind, extract_file_changes, FileChange};

/// 路由条目契约 re-export（execute_command RPC 域/等级检查消费）。
pub use peri_acp_types::command::command_route::RouteEntry;
/// 词法等级 re-export（Immediate 语义 = 第一等级检查）。
pub use peri_acp_types::command::CommandLevel;
/// 命令契约（L5：事实源 peri-acp-types::command；`AgentCommand` /
/// `CommandKind` 已随 Phase 5 Step 6 删除）。
pub use peri_acp_types::command::{
    CommandContext, CommandFeedback, CommandResult, FeedbackChannel, FeedbackLevel,
    PromptStopReason,
};

/// 注册表契约（Phase 2 Step 4 换型：本模块不再持有 Vec 实现，注册表本体在
/// 契约层 `peri-acp-types::command_registry`；Phase 3/5 消费方经本路径引用）。
pub use peri_acp_types::command_registry::{CommandRegistry, RegisterError, ResolvedCommand};

/// 内置命令条目辅助构造（组合根，Phase 5 Step 6：旧 AgentCommand 转发 /
/// LegacyAdapter 已删除）：`fullname = "core:{name}"`，aliases / description
/// 取命令实现的关联常量（命令声明，单一事实源）；args_schema 由调用点传入
/// （Phase 5 逐命令补——命令迁移时声明自己的参数形态，未迁移命令传 None）。
pub fn handler_entry<C: CommandHandler + Default + 'static>(
    fullname: &str,
    aliases: &'static [&'static str],
    description: &'static str,
    args_schema: Option<ArgsSchema>,
) -> RouteEntry {
    RouteEntry {
        fullname: fullname.to_string(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        description: description.to_string(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema,
        handler: Arc::new(C::default()),
        provenance: CommandProvenance {
            source: CommandSource::Core,
            lifecycle: CommandLifecycle::Connected,
        },
    }
}

/// `core:loop` 占位 handler（Phase 5「loop 命令迁移」替换为正式实现）：
/// Done + UI-only 反馈，保证投影条目不缺失、路由确定性执行。
#[derive(Default)]
struct LoopPlaceholder;

impl LoopPlaceholder {
    /// 描述（注册条目挂载；与现状投影硬编码一致，dispatch/commands.rs）。
    const DESCRIPTION: &'static str = "Control agent iteration loop";
}

#[async_trait]
impl CommandHandler for LoopPlaceholder {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        CommandOutcome::Done(CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Info,
                message: "loop 命令尚未实现".to_string(),
                channel: FeedbackChannel::UiOnly,
            }),
        })
    }
}

/// `core:{skill}` 本地 skill 注入占位 handler（Phase 6 C1；设计 §71
/// Inject 三态：透传指令进正常 agent 管线）。
///
/// skill 注入语义 = 把 skill 调用指令文本注入 agent 管线；当前为最小占位
/// ——返回 [`CommandOutcome::Inject`] **用户消息原文**（整段透传，含
/// `/skill-name` token）：原文进 agent 管线，Skills 中间件旧路径
/// （SkillPreloadMiddleware 自动检测）继续处理，命令不被吞。
/// 完整语义（按 skill 名加载 SKILL.md 注入系统提示）留待后续版本。
#[derive(Clone, Copy)]
pub struct AgentPassthrough;

#[async_trait]
impl CommandHandler for AgentPassthrough {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        // 原文整段交还 agent 管线（含 `/skill-name` token）：SkillPreload
        // 中间件自动检测分支依赖原文，命令不被吞。RPC 路径（execute-command）
        // 无 agent 管线，Inject 由调用方显式报错（execute_command.rs 语义）。
        CommandOutcome::Inject(ctx.raw_text)
    }
}

/// 内置命令注册（core 域；会话创建时调用一次；注册顺序即优先级——先注册者
/// 占键，后注册同键一律 Conflict 拒绝，设计 §64 纯拒绝 + 装配顺序裁决）。
///
/// 内置 fullname 与 alias 均为词法合法且互不冲突的常量，注册失败即编程错误。
pub fn register_builtins(reg: &CommandRegistry) {
    // Phase 5 Step 4：compact 已迁移新契约（CommandHandler 主实现），无参命令，
    // args_schema 挂 ArgsSchema::default()（投影可渲染）。
    reg.register(handler_entry::<compact::CompactCommand>(
        "core:compact",
        compact::CompactCommand::ALIASES,
        compact::CompactCommand::DESCRIPTION,
        Some(ArgsSchema::default()),
    ))
    .expect("core:compact 注册失败：词法合法且无冲突，失败即编程错误");
    // Phase 5 Step 3：clear 已迁移新契约（CommandHandler 主实现），无参命令，
    // args_schema 挂 ArgsSchema::default()（投影可渲染）。
    reg.register(handler_entry::<clear::ClearCommand>(
        "core:clear",
        clear::ClearCommand::ALIASES,
        clear::ClearCommand::DESCRIPTION,
        Some(ArgsSchema::default()),
    ))
    .expect("core:clear 注册失败：词法合法且无冲突，失败即编程错误");
    // Phase 5 Step 5：rewind 已迁移新契约（CommandHandler 主实现），参数形态
    // 由 RewindCommand::args_schema() 声明（positionals: [target_message_id]，
    // flags: [--no-revert-files]；slash 形态 /rewind <target_message_id>
    // [--no-revert-files]；RPC 路径 wire 保持 JSON 不变）。
    reg.register(handler_entry::<rewind::RewindCommand>(
        "core:rewind",
        rewind::RewindCommand::ALIASES,
        rewind::RewindCommand::DESCRIPTION,
        Some(rewind::RewindCommand::args_schema()),
    ))
    .expect("core:rewind 注册失败：词法合法且无冲突，失败即编程错误");
    // loop：无现执行体（现状仅投影幽灵条目，dispatch/commands.rs 硬编码），
    // 预注册占位 handler（LoopPlaceholder：Done + UI-only 反馈「loop 命令
    // 尚未实现」），保证 Phase 3 投影条目不缺失；Phase 5「loop 命令迁移」
    // 小节替换为正式实现（P1-7）。
    reg.register(handler_entry::<LoopPlaceholder>(
        "core:loop",
        &[],
        LoopPlaceholder::DESCRIPTION,
        None,
    ))
    .expect("core:loop 注册失败：词法合法且无冲突，失败即编程错误");
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
