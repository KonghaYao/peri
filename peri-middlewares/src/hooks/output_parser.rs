use crate::hooks::types::{
    HookAction, HookDecision, HookSpecificOutput, PermissionDecision, SyncHookResponse,
};

/// 解析 command hook stdout 输出
///
/// 对齐 Claude Code parseHookOutput + processHookJSONOutput:
/// - 不以 `{` 开头 → 纯文本输出，视为 Allow
/// - 以 `{` 开头 → 尝试解析为 SyncHookResponse JSON
pub fn parse_command_hook_output(stdout: &str) -> HookAction {
    let trimmed = stdout.trim();

    // 不以 { 开头 → 纯文本输出，视为 Allow
    if !trimmed.starts_with('{') {
        return HookAction::Allow;
    }

    // 尝试解析为 SyncHookResponse JSON
    match serde_json::from_str::<SyncHookResponse>(trimmed) {
        Ok(response) => sync_response_to_action(&response),
        Err(e) => {
            // JSON 解析失败 → 纯文本，视为 Allow（记录日志）
            tracing::warn!("Hook stdout JSON parse failed: {}", e);
            HookAction::Allow
        }
    }
}

/// 解析 HTTP hook 响应
///
/// 对齐 Claude Code parseHttpHookOutput：
/// - 空 body → 视为 {}（有效 JSON）
/// - 不以 `{` 开头 → 非法（HTTP hook 必须返回 JSON）
pub fn parse_http_hook_response(body: &str) -> HookAction {
    let trimmed = body.trim();

    // 空 body → 视为 {}（有效 JSON）
    if trimmed.is_empty() {
        return HookAction::Allow;
    }

    // 不以 { 开头 → 非法（HTTP hook 必须返回 JSON）
    if !trimmed.starts_with('{') {
        // 只记录长度与类别，不记录 body 正文（H4：日志不得携带 hook 原文）
        tracing::warn!(
            bytes = trimmed.len(),
            "HTTP hook must return JSON, got non-JSON body"
        );
        return HookAction::Allow;
    }

    match serde_json::from_str::<SyncHookResponse>(trimmed) {
        Ok(response) => sync_response_to_action(&response),
        Err(e) => {
            tracing::warn!("HTTP hook JSON parse failed: {}", e);
            HookAction::Allow
        }
    }
}

/// 将 SyncHookResponse 转换为内部 HookAction
///
/// 优先级（严格按顺序）：
/// 1. continue=false → PreventContinuation
/// 2. decision=block → Block
/// 3. PreToolUse 组合输出 → PermissionOverride（判定/updatedInput/context/systemMessage 全保留）
/// 4. systemMessage → SystemMessage
/// 5. hookSpecificOutput → 事件特定处理
/// 6. 以上都不满足 → Allow
fn sync_response_to_action(response: &SyncHookResponse) -> HookAction {
    // 1. continue=false → 阻止继续
    if response.continue_run == Some(false) {
        return HookAction::PreventContinuation {
            stop_reason: response.stop_reason.clone(),
        };
    }

    // 2. decision=block → 阻止操作
    if response.decision == Some(HookDecision::Block) {
        return HookAction::Block {
            reason: response
                .reason
                .clone()
                .unwrap_or_else(|| "Blocked by hook".into()),
        };
    }

    // 3. PreToolUse 组合输出：deny/ask/allow 判定与 updatedInput/附加上下文/系统消息
    //    同时保留，禁止「取 updatedInput 丢 decision」式的互相吞并
    if let Some(combined) = pretooluse_combination(response) {
        return combined;
    }

    // 4. systemMessage → 注入系统消息
    if let Some(ref msg) = response.system_message {
        return HookAction::SystemMessage {
            message: msg.clone(),
        };
    }

    // 5. hookSpecificOutput → 事件特定处理
    if let Some(ref specific) = response.hook_specific_output {
        return hook_specific_to_action(specific);
    }

    HookAction::Allow
}

/// PreToolUse 组合输出归并。
///
/// 只要有判定、附加上下文或系统消息参与，就返回携带全部字段的
/// [`HookAction::PermissionOverride`]；仅 `updatedInput` 且无其它字段时返回 `None`，
/// 保持既有 [`HookAction::ModifyInput`] 语义（不改动只改写参数的 hook 行为）。
fn pretooluse_combination(response: &SyncHookResponse) -> Option<HookAction> {
    let HookSpecificOutput::PreToolUse {
        permission_decision,
        permission_decision_reason,
        updated_input,
        additional_context,
    } = response.hook_specific_output.as_ref()?
    else {
        return None;
    };

    // 无任何 PreToolUse 特有字段 → 保持既有 systemMessage 优先级
    if permission_decision.is_none() && updated_input.is_none() && additional_context.is_none() {
        return None;
    }
    // 仅 updatedInput → 既有 ModifyInput 语义
    if permission_decision.is_none()
        && additional_context.is_none()
        && response.system_message.is_none()
    {
        return None;
    }

    Some(HookAction::PermissionOverride {
        decision: permission_decision
            .clone()
            .unwrap_or(PermissionDecision::Passthrough),
        reason: permission_decision_reason.clone(),
        updated_input: updated_input.clone(),
        additional_context: additional_context.clone(),
        system_message: response.system_message.clone(),
    })
}

/// 将 HookSpecificOutput 转换为内部 HookAction（非 PreToolUse 组合路径）
fn hook_specific_to_action(specific: &HookSpecificOutput) -> HookAction {
    match specific {
        HookSpecificOutput::PreToolUse {
            updated_input: Some(input),
            ..
        } => HookAction::ModifyInput {
            new_input: input.clone(),
        },
        HookSpecificOutput::UserPromptSubmit {
            additional_context: Some(ctx),
            ..
        } => HookAction::AdditionalContext {
            context: ctx.clone(),
        },
        HookSpecificOutput::SessionStart {
            initial_user_message: Some(msg),
            ..
        } => HookAction::InitialUserMessage {
            message: msg.clone(),
        },
        HookSpecificOutput::SessionStart {
            additional_context: Some(ctx),
            ..
        } => HookAction::AdditionalContext {
            context: ctx.clone(),
        },
        _ => HookAction::Allow,
    }
}

#[cfg(test)]
#[path = "output_parser_test.rs"]
mod tests;
