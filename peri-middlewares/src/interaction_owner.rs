//! 交互归属（[`TrustedInteractionOwner`]）的铸造规则——**单一权威**。
//!
//! 审批（`PermissionMiddleware` / HookMiddleware 的 `ask`）与提问
//! （`AskUserTool`）都要在调用 broker 前给出「谁在问」。归属只能从真实执行
//! 事实铸造：
//!
//! - middleware hook：`BeforeToolState::interaction_attribution()`
//!   （canonical 阶段投影的 [`InteractionAttribution`]，见 `BoundApprovalState`）；
//! - 工具调用：`ToolContext::attribution`（canonical dispatch 绑定）。
//!
//! 归属 → [`TrustedInteractionOwner`] 的映射只有一条实现（`owner()`）：
//! 本模块不做第二次推导、不拼装、不从 session / turn 字段反推。
//!
//! 拿不到事实时**不铸造、不伪造**：调用方必须显式拒绝该交互（返回
//! [`untrusted_owner_error`]），不得静默跳过审批，也不得降级为自动批准。
//!
//! 本模块不做路由决策：归属是给 session 交互路由 / ACP endpoint 的输入。

use peri_acp_types::interaction::{InteractionAttribution, TrustedInteractionOwner};
use peri_agent::error::AgentError;
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::tools::ToolContext;

/// 无可信归属时的拒绝原因（审批与提问共用同一措辞）。
///
/// 语义是「本次交互无法归属到可信 owner，因此不可路由 ⇒ 未执行」，既不是
/// 用户拒绝，也不是客户端无人可作答。
pub(crate) const UNTRUSTED_OWNER_REASON: &str =
    "交互缺少可信归属（当前执行没有 session / turn 事实），无法送达任何审批 endpoint，操作未执行";

/// 没有可信归属时的显式拒绝（fail-closed，不静默放行）。
pub(crate) fn untrusted_owner_error(tool: &str) -> AgentError {
    AgentError::ToolRejected {
        tool: tool.to_string(),
        reason: UNTRUSTED_OWNER_REASON.to_string(),
    }
}

/// 从审批 hook 状态铸造归属；状态没有可信归属事实时返回 `None`。
///
/// 事实源是 [`hook_state::StateView::interaction_attribution`]（canonical 阶段投影
/// 的 [`InteractionAttribution`]）：归属 → owner 只有一条映射（`owner()`），
/// 本函数不自行拼装、不从 session / turn 字段二次推导。
pub(crate) fn owner_from_hook_state(
    state: &dyn hook_state::BeforeToolState,
) -> Option<TrustedInteractionOwner> {
    state.interaction_attribution().as_ref().map(|a| a.owner())
}

/// 从工具调用上下文铸造归属；缺少归属事实时返回 `None`。
///
/// 事实由 canonical dispatch 绑定（`ToolContext::with_attribution`）：模型批次取
/// 真实 turn 事实，App / Host 入口从 typed 来源派生。工具自己不生成归属。
pub(crate) fn owner_from_tool_context(ctx: &ToolContext<'_>) -> Option<TrustedInteractionOwner> {
    ctx.attribution.as_ref().map(InteractionAttribution::owner)
}

#[cfg(test)]
#[path = "interaction_owner_test.rs"]
mod interaction_owner_tests;
