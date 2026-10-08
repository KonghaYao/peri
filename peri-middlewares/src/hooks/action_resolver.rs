//! Hook Action 归约：把 `HookAction` 统一转换成 `AgentResult`。
//!
//! 消除 middleware trait 实现中 5 处重复出现的 `match action { Block/PreventContinuation
//! => Err(AgentError::ToolRejected), ModifyInput => ToolCall, _ => {} }` 模板。
//! 保持每个调用点的语义与原实现一致，包括默认 reason 文案。

use peri_agent::{
    agent::react::{ToolCall, ToolResult},
    error::{AgentError, AgentResult},
};

use crate::hooks::types::HookAction;

/// Action 到 `AgentResult<()>` 的归约：Block / PreventContinuation 转
/// `AgentError::ToolRejected`，其余视为放行。
///
/// - `tool`: 用于填充 `AgentError::ToolRejected.tool`，一般是触发事件名
///   （如 `"PreToolUse"` / `"PostToolBatch"`）或具体 tool 名（如 `"Bash"`）。
/// - `fallback_reason`: PreventContinuation 在 `stop_reason == None` 时使用的兜底文案。
pub fn resolve_action_to_result(
    action: &HookAction,
    tool: &str,
    fallback_reason: &str,
) -> AgentResult<()> {
    match action {
        HookAction::Block { reason } => Err(AgentError::ToolRejected {
            tool: tool.to_string(),
            reason: reason.clone(),
        }),
        HookAction::PreventContinuation { stop_reason } => Err(AgentError::ToolRejected {
            tool: tool.to_string(),
            reason: stop_reason
                .clone()
                .unwrap_or_else(|| fallback_reason.to_string()),
        }),
        _ => Ok(()),
    }
}

/// Action 到 `ToolCall` 的归约：处理 `ModifyInput`（替换 input 后返回新 ToolCall），
/// Block / PreventContinuation 转 `AgentError::ToolRejected`，其余放行原 `tool_call`。
///
/// 用于 `before_tool` 中 PreToolUse / PermissionRequest 分支，消除两处几乎完全
/// 相同的 `match action` 块。
pub fn resolve_action_to_toolcall(
    action: &HookAction,
    tool_call: &ToolCall,
    fallback_reason: &str,
) -> AgentResult<ToolCall> {
    match action {
        HookAction::Block { reason } => Err(AgentError::ToolRejected {
            tool: tool_call.name.clone(),
            reason: reason.clone(),
        }),
        HookAction::PreventContinuation { stop_reason } => Err(AgentError::ToolRejected {
            tool: tool_call.name.clone(),
            reason: stop_reason
                .clone()
                .unwrap_or_else(|| fallback_reason.to_string()),
        }),
        HookAction::ModifyInput { new_input } => Ok(ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            input: new_input.clone(),
        }),
        _ => Ok(tool_call.clone()),
    }
}

/// PostToolBatch 的阻断语义决策。
///
/// 工具结果此时已提交：Block 不是拒绝（不得伪装 `ToolRejected`），而是回注有界
/// 反馈让模型修正；`continue:false` 是显式停止意图，经 Receive 唯一退出口停止。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostToolBatchDecision {
    /// 无阻断动作：正常继续。
    Continue,
    /// Block：回注有界反馈（防循环计数由调用方复用 stop_block_guard）。
    Feedback { reason: String },
    /// continue:false：显式停止意图，不发起额外模型请求。
    Stop { stop_reason: Option<String> },
}

/// 把 PostToolBatch hook 的归并 action 映射成执行语义决策。
pub fn resolve_post_tool_batch_action(action: &HookAction) -> PostToolBatchDecision {
    match action {
        HookAction::Block { reason } => PostToolBatchDecision::Feedback {
            reason: reason.clone(),
        },
        HookAction::PreventContinuation { stop_reason } => PostToolBatchDecision::Stop {
            stop_reason: stop_reason.clone(),
        },
        _ => PostToolBatchDecision::Continue,
    }
}

/// 把 `ToolResult.output` 转成 hook 使用的 `serde_json::Value`（透传字符串）。
pub fn tool_output_to_json(result: &ToolResult) -> serde_json::Value {
    serde_json::json!(result.output)
}
