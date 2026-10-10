//! Hook 结果归并契约（红→绿）。
//!
//! 归并必须对"判定方在左/在右"对称：先出现的 Deny/Ask/ModifyInput/additionalContext
//! 不得被后一个"无判定"输出（Allow / systemMessage / …）覆盖丢失。
//! 行为用例走真实 `HookMiddleware::before_tool` 链路：Bypass 模式下 deny 与
//! 无审批端口的 ask 同样零执行。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::hooks::middleware::HookMiddleware;
use crate::hooks::types::{HookAction, HookEvent, HookType, PermissionDecision, RegisteredHook};
use crate::permission::{PermissionMode, SharedPermissionMode};
use peri_agent::agent::react::ToolCall;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::middleware::r#trait::Middleware;

// === 直接归并（merge_hook_actions）断言 ===

fn override_action(
    decision: PermissionDecision,
    updated_input: Option<serde_json::Value>,
    additional_context: Option<&str>,
    system_message: Option<&str>,
) -> HookAction {
    HookAction::PermissionOverride {
        decision,
        reason: None,
        updated_input,
        additional_context: additional_context.map(str::to_string),
        system_message: system_message.map(str::to_string),
    }
}

fn assert_override<'a>(
    action: &'a HookAction,
    expected_decision: PermissionDecision,
    label: &str,
) -> &'a HookAction {
    match action {
        HookAction::PermissionOverride { decision, .. } => {
            assert_eq!(decision, &expected_decision, "{label}");
            action
        }
        other => panic!("{label}: 期望 PermissionOverride，got {other:?}"),
    }
}

/// [deny, allow]：后一个 hook 无判定不得抹掉先出现的 deny。
#[test]
fn merge_deny_then_allow_keeps_deny() {
    let merged = merge_hook_actions(
        override_action(PermissionDecision::Deny, None, None, None),
        HookAction::Allow,
    );
    assert_override(
        &merged,
        PermissionDecision::Deny,
        "[deny, allow] 必须保持 deny",
    );
}

#[test]
fn merge_deny_then_initial_user_message_keeps_deny() {
    let merged = merge_hook_actions(
        override_action(PermissionDecision::Deny, None, None, None),
        HookAction::InitialUserMessage {
            message: "foreign output".to_string(),
        },
    );
    assert_override(
        &merged,
        PermissionDecision::Deny,
        "deny must survive foreign output",
    );
}

#[test]
fn merge_ask_then_initial_user_message_keeps_ask() {
    let merged = merge_hook_actions(
        override_action(PermissionDecision::Ask, None, None, None),
        HookAction::InitialUserMessage {
            message: "foreign output".to_string(),
        },
    );
    assert_override(
        &merged,
        PermissionDecision::Ask,
        "ask must survive foreign output",
    );
}

/// [allow, deny]：对称方向，deny 同样胜出。
#[test]
fn merge_allow_then_deny_keeps_deny() {
    let merged = merge_hook_actions(
        HookAction::Allow,
        override_action(PermissionDecision::Deny, None, None, None),
    );
    assert_override(
        &merged,
        PermissionDecision::Deny,
        "[allow, deny] 必须保持 deny",
    );
}

/// [deny, systemMessage]：无判定的 systemMessage 必须被吸收，deny 不被覆盖。
#[test]
fn merge_deny_then_system_message_keeps_deny_and_message() {
    let merged = merge_hook_actions(
        override_action(PermissionDecision::Deny, None, None, None),
        HookAction::SystemMessage {
            message: "client hint".to_string(),
        },
    );
    let action = assert_override(&merged, PermissionDecision::Deny, "[deny, systemMessage]");
    match action {
        HookAction::PermissionOverride { system_message, .. } => {
            assert_eq!(system_message.as_deref(), Some("client hint"))
        }
        _ => unreachable!(),
    }
}

/// [ask, allow]：ask 不得被后一个无判定输出降级为放行。
#[test]
fn merge_ask_then_allow_keeps_ask() {
    let merged = merge_hook_actions(
        override_action(PermissionDecision::Ask, None, None, None),
        HookAction::Allow,
    );
    assert_override(
        &merged,
        PermissionDecision::Ask,
        "[ask, allow] 必须保持 ask",
    );
}

/// [ModifyInput, Allow]：先出现的 updatedInput 不得被后一个无判定输出丢弃。
#[test]
fn merge_modify_input_then_allow_keeps_updated_input() {
    let merged = merge_hook_actions(
        HookAction::ModifyInput {
            new_input: serde_json::json!({"command": "echo safe"}),
        },
        HookAction::Allow,
    );
    match merged {
        HookAction::ModifyInput { new_input } => {
            assert_eq!(new_input, serde_json::json!({"command": "echo safe"}))
        }
        other => panic!("[ModifyInput, Allow] 必须保持 ModifyInput，got {other:?}"),
    }
}

/// [SystemMessage, deny]：先出现的 systemMessage 必须在 deny 判定里保留。
#[test]
fn merge_system_message_then_deny_absorbs_message() {
    let merged = merge_hook_actions(
        HookAction::SystemMessage {
            message: "earlier note".to_string(),
        },
        override_action(PermissionDecision::Deny, None, None, None),
    );
    let action = assert_override(&merged, PermissionDecision::Deny, "[SystemMessage, deny]");
    match action {
        HookAction::PermissionOverride { system_message, .. } => {
            assert_eq!(system_message.as_deref(), Some("earlier note"))
        }
        _ => unreachable!(),
    }
}

/// [ModifyInput, systemMessage]：两类字段并存时都不得丢失（以 Passthrough 承载）。
#[test]
fn merge_modify_input_and_system_message_keeps_both_fields() {
    let merged = merge_hook_actions(
        HookAction::ModifyInput {
            new_input: serde_json::json!({"command": "echo safe"}),
        },
        HookAction::SystemMessage {
            message: "note".to_string(),
        },
    );
    match merged {
        HookAction::PermissionOverride {
            decision,
            updated_input,
            system_message,
            ..
        } => {
            assert_eq!(decision, PermissionDecision::Passthrough);
            assert_eq!(
                updated_input,
                Some(serde_json::json!({"command": "echo safe"}))
            );
            assert_eq!(system_message.as_deref(), Some("note"));
        }
        other => panic!("[ModifyInput, systemMessage] 两类字段都必须保留，got {other:?}"),
    }
}

/// additionalContext 必须按顺序拼接，不得后者覆盖前者。
#[test]
fn merge_additional_context_is_concatenated() {
    let merged = merge_hook_actions(
        HookAction::AdditionalContext {
            context: "first".to_string(),
        },
        HookAction::AdditionalContext {
            context: "second".to_string(),
        },
    );
    match merged {
        HookAction::AdditionalContext { context } => assert_eq!(context, "first\nsecond"),
        other => panic!("additionalContext 必须拼接，got {other:?}"),
    }
}

// === 行为用例：真实 middleware 链路（Bypass 模式零执行） ===

fn hook_with_command(event: HookEvent, command: &str) -> RegisteredHook {
    RegisteredHook {
        hook: HookType::Command {
            command: command.to_string(),
            shell: None,
            timeout: Some(1000),
            status_message: None,
            once: false,
            async_run: false,
            async_rewake: false,
            matcher: None,
            condition: None,
        },
        event,
        matcher: None,
        plugin_name: "merge-contract-plugin".to_string(),
        plugin_id: "merge-contract-plugin-id".to_string(),
        plugin_source: None,
        plugin_root: PathBuf::from("/tmp/merge-contract-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/merge-contract-plugin-data"),
        plugin_options: HashMap::new(),
    }
}

fn json_hook(event: HookEvent, json: &str) -> RegisteredHook {
    hook_with_command(event, &format!("echo '{json}'"))
}

/// 无判定 hook：纯文本 stdout（不以 `{` 开头）→ Allow。
fn no_opinion_hook(event: HookEvent) -> RegisteredHook {
    hook_with_command(event, "true")
}

fn make_middleware(hooks: Vec<RegisteredHook>) -> HookMiddleware {
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!("no LLM needed in merge-contract tests"));
    HookMiddleware::new(
        hooks,
        llm_factory,
        std::env::temp_dir().to_str().unwrap(),
        "merge-contract-session",
        "/test/transcript.json",
        SharedPermissionMode::new(PermissionMode::Bypass),
        "opus",
    )
}

async fn before_tool(mw: &HookMiddleware, call: &ToolCall) -> AgentResult<ToolCall> {
    mw.before_tool(
        &mut peri_agent::agent::state::AgentState::new(std::env::temp_dir().to_str().unwrap()),
        call,
    )
    .await
}

fn bash_call(input: serde_json::Value) -> ToolCall {
    ToolCall::new("c1", "Bash", input)
}

/// [deny, allow]：Bypass 模式下 deny 仍必须零执行。
#[cfg(unix)]
#[tokio::test]
async fn pretooluse_deny_then_allow_is_zero_execution_in_bypass() {
    let deny = json_hook(
        HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#,
    );
    let mw = make_middleware(vec![deny, no_opinion_hook(HookEvent::PreToolUse)]);

    let result = before_tool(&mw, &bash_call(serde_json::json!({"command": "rm -rf /"}))).await;

    match result {
        Err(AgentError::ToolRejected { tool, .. }) => assert_eq!(tool, "Bash"),
        other => panic!("[deny, allow] 必须零执行，got {other:?}"),
    }
}

/// [deny, systemMessage]：无判定 systemMessage 不得覆盖 deny，且正文不得回显。
#[cfg(unix)]
#[tokio::test]
async fn pretooluse_deny_then_system_message_is_zero_execution_in_bypass() {
    let deny = json_hook(
        HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}"#,
    );
    let note = json_hook(HookEvent::PreToolUse, r#"{"systemMessage":"secret-note"}"#);
    let mw = make_middleware(vec![deny, note]);

    let result = before_tool(&mw, &bash_call(serde_json::json!({"command": "rm -rf /"}))).await;

    match result {
        Err(AgentError::ToolRejected { reason, .. }) => {
            assert!(
                !reason.contains("secret-note"),
                "拒绝反馈不得携带 hook 自述正文: {reason}"
            );
        }
        other => panic!("[deny, systemMessage] 必须零执行，got {other:?}"),
    }
}

/// [ask, allow]：Bypass 模式无审批端口时 ask 必须拒绝，不得被 allow 降级放行。
#[cfg(unix)]
#[tokio::test]
async fn pretooluse_ask_then_allow_without_broker_is_rejected() {
    let ask = json_hook(
        HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
    );
    let mw = make_middleware(vec![ask, no_opinion_hook(HookEvent::PreToolUse)]);

    let result = before_tool(&mw, &bash_call(serde_json::json!({"command": "ls"}))).await;

    assert!(
        matches!(result, Err(AgentError::ToolRejected { .. })),
        "[ask, allow] 无审批端口必须拒绝，got {result:?}"
    );
}

/// [ModifyInput, Allow]：先出现的 updatedInput 必须继续生效。
#[cfg(unix)]
#[tokio::test]
async fn pretooluse_modify_input_then_allow_applies_updated_input() {
    let modify = json_hook(
        HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","updatedInput":{"command":"echo safe"}}}"#,
    );
    let mw = make_middleware(vec![modify, no_opinion_hook(HookEvent::PreToolUse)]);

    let result = before_tool(&mw, &bash_call(serde_json::json!({"command": "rm -rf /"}))).await;

    let call = result.expect("updatedInput 必须生效");
    assert_eq!(call.input, serde_json::json!({"command": "echo safe"}));
}
