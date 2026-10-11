//! M10 hook 输出字段路由：把已解析的 hook 输出投递到各自受众面。
//!
//! - `additionalContext` → 有界、带 hook 来源的 Model reminder（不唤醒新一轮）；
//! - `systemMessage` → 客户端提示（Tui 受众，不进模型上下文）；
//! - 没有投递面的调用点（直接 `fire_event`、只读阶段）→ 只记录字段名与长度。
//!
//! 从 `hooks/middleware.rs` 按职责拆出（STD-SIZE-001）：中间件本体只保留
//! [`super::middleware::HookMiddleware`] 的门面与阶段编排，受众投递规则集中在本
//! 模块一处，生产阶段方法经 `HookMiddleware::fire_event_with_delivery` 复用同一实现。

use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_agent::error::AgentError;
use peri_agent::middleware::capabilities as hook_state;
use serde_json::json;

use crate::hooks::types::{HookAction, HookEvent};

/// hook 输出正文的承载预算（UTF-8 字节）：超限按字符边界截断并显式标记，
/// 不静默裁掉内容。
pub(crate) const MAX_HOOK_OUTPUT_BYTES: usize = 32 * 1024;

/// 按字符边界把 hook 输出限制在承载预算内，并在截断时留下显式说明。
fn bounded_hook_output(text: &str) -> String {
    if text.len() <= MAX_HOOK_OUTPUT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_HOOK_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[hook output truncated: {} of {} bytes kept]",
        &text[..end],
        end,
        text.len()
    )
}

/// 事件名 → 稳定的 kind 片段（程序路由匹配 source + kind，不用展示文本）。
///
/// kind 是程序路由事实源，因此逐事件显式枚举：新增事件必须在此声明，未知事件
/// 也保留带原始事件名的可路由身份，不允许悄悄落进兜底分支。
fn event_kind(event: &HookEvent) -> String {
    match event {
        HookEvent::PreToolUse => "pre_tool_use",
        HookEvent::PostToolUse => "post_tool_use",
        HookEvent::PostToolUseFailure => "post_tool_use_failure",
        HookEvent::PostToolBatch => "post_tool_batch",
        HookEvent::PermissionRequest => "permission_request",
        HookEvent::PermissionDenied => "permission_denied",
        HookEvent::UserPromptSubmit => "user_prompt_submit",
        HookEvent::SessionStart => "session_start",
        HookEvent::SessionEnd => "session_end",
        HookEvent::Stop => "stop",
        HookEvent::StopFailure => "stop_failure",
        HookEvent::SubagentStart => "subagent_start",
        HookEvent::SubagentStop => "subagent_stop",
        HookEvent::PreCompact => "pre_compact",
        HookEvent::PostCompact => "post_compact",
        HookEvent::Notification => "notification",
        HookEvent::Setup => "setup",
        HookEvent::TeammateIdle => "teammate_idle",
        HookEvent::TaskCreated => "task_created",
        HookEvent::TaskCompleted => "task_completed",
        HookEvent::ConfigChange => "config_change",
        HookEvent::WorktreeCreate => "worktree_create",
        HookEvent::WorktreeRemove => "worktree_remove",
        HookEvent::InstructionsLoaded => "instructions_loaded",
        HookEvent::Elicitation => "elicitation",
        HookEvent::ElicitationResult => "elicitation_result",
        HookEvent::CwdChanged => "cwd_changed",
        HookEvent::FileChanged => "file_changed",
        HookEvent::Unknown(name) => return format!("unknown:{name}"),
    }
    .to_string()
}

/// M10 字段路由：把 hook 输出投递到其声明受众（单一实现，供各阶段共用）。
///
/// - `additionalContext` → 有界、带 hook 来源的 Model reminder（不唤醒新一轮）；
/// - `systemMessage` → 客户端提示（Tui 受众，不进模型上下文）；
/// - 无法在可承载预算内构造的字段：显式诊断，不静默丢弃、不假装生效。
pub(crate) fn route_hook_output(
    state: &dyn hook_state::HookOutputState,
    event: &HookEvent,
    action: &HookAction,
) {
    let (additional_context, system_message) = match action {
        HookAction::AdditionalContext { context } => (Some(context.as_str()), None),
        HookAction::SystemMessage { message } => (None, Some(message.as_str())),
        HookAction::PermissionOverride {
            additional_context,
            system_message,
            ..
        } => (additional_context.as_deref(), system_message.as_deref()),
        _ => (None, None),
    };
    let kind = event_kind(event);

    if let Some(context) = additional_context {
        match hook_output_reminder(
            &kind,
            "additional_context",
            context,
            &[ReminderAudience::Model],
        ) {
            Ok(reminder) => state.enqueue_hook_model_reminder(reminder),
            Err(error) => tracing::warn!(
                event = ?event,
                field = "additionalContext",
                %error,
                "hook additionalContext could not be delivered as a model reminder"
            ),
        }
    }

    if let Some(message) = system_message {
        match hook_output_reminder(&kind, "system_message", message, &[ReminderAudience::Tui]) {
            Ok(reminder) => state.enqueue_hook_client_notice(reminder),
            Err(error) => tracing::warn!(
                event = ?event,
                field = "systemMessage",
                %error,
                "hook systemMessage could not be delivered as a client notice"
            ),
        }
    }
}

/// 构造一条 hook 输出 reminder：正文有界、来源为 hook、受众显式声明。
pub(crate) fn hook_output_reminder(
    kind: &str,
    field: &str,
    text: &str,
    audiences: &[ReminderAudience],
) -> Result<TrustedSystemReminder, AgentError> {
    let body = bounded_hook_output(text);
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Guidance,
            source: ReminderSource("hook".into()),
            kind: format!("{kind}_{field}"),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(audiences.to_vec()),
            body,
            summary: Some(format!("hook {kind} {field}")),
            metadata: json!({
                "event": kind,
                "field": field,
                "bytes": text.len(),
            }),
        })
        .map_err(|error| AgentError::MiddlewareError {
            middleware: "HookMiddleware".to_string(),
            reason: format!("hook output reminder rejected: {error}"),
        })
}

/// 已解析但投递尚未接线的 hook 输出：只记录事件、字段名与长度，绝不记录正文。
///
/// 仅在**没有投递面**的调用点使用（直接调用 `fire_event` 的用法、或只读阶段
/// 如 StopFailure）。生产阶段方法必须经
/// [`HookMiddleware::fire_event_with_delivery`] 走真实投递；未接线期间不得静默
/// 丢弃，也不得"解析成功即假装生效"。
pub(crate) fn diagnose_undelivered_output(event: &HookEvent, action: &HookAction) {
    let (additional_context, system_message) = match action {
        HookAction::AdditionalContext { context } => (Some(context.as_str()), None),
        HookAction::SystemMessage { message } => (None, Some(message.as_str())),
        HookAction::PermissionOverride {
            additional_context,
            system_message,
            ..
        } => (additional_context.as_deref(), system_message.as_deref()),
        _ => (None, None),
    };
    if let Some(context) = additional_context {
        tracing::debug!(
            event = ?event,
            field = "additionalContext",
            bytes = context.len(),
            "hook output parsed but delivery is not wired yet (M10 → F)"
        );
    }
    if let Some(message) = system_message {
        tracing::debug!(
            event = ?event,
            field = "systemMessage",
            bytes = message.len(),
            "hook output parsed but delivery is not wired yet (M10 → F)"
        );
    }
}
