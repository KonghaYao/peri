use super::*;
use crate::hooks::types::PermissionDecision;

// === parse_command_hook_output tests ===

#[test]
fn test_parse_command_plain_text() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, "hello world"),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_command_continue_false() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, r#"{"continue": false}"#),
        HookAction::PreventContinuation { stop_reason: None }
    ));
}

#[test]
fn test_parse_command_decision_block() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, r#"{"decision": "block", "reason": "test"}"#),
        HookAction::Block { reason } if reason == "test"
    ));
}

#[test]
fn test_parse_command_system_message() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, r#"{"systemMessage": "warning"}"#),
        HookAction::SystemMessage { message } if message == "warning"
    ));
}

#[test]
fn test_parse_command_invalid_json() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, "{invalid json}"),
        HookAction::Block { .. }
    ));
}

#[test]
fn test_parse_command_empty() {
    assert!(matches!(
        parse_command_hook_output(&HookEvent::PreToolUse, ""),
        HookAction::Allow
    ));
}

// === parse_http_hook_response tests ===

#[test]
fn test_parse_http_empty_body() {
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PreToolUse, ""),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_http_whitespace_body() {
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PreToolUse, "   "),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_http_non_json_body() {
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PreToolUse, "plain text"),
        HookAction::Block { .. }
    ));
}

#[test]
fn test_parse_http_valid_json() {
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PreToolUse, r#"{"continue": false, "stopReason": "test"}"#),
        HookAction::PreventContinuation { stop_reason } if stop_reason.as_deref() == Some("test")
    ));
}

#[test]
fn test_parse_http_invalid_json() {
    assert!(matches!(
        parse_http_hook_response(&HookEvent::PreToolUse, "{invalid}"),
        HookAction::Block { .. }
    ));
}

// === sync_response_to_action tests ===

#[test]
fn test_sync_response_priority_continue_over_decision() {
    let resp = SyncHookResponse {
        continue_run: Some(false),
        decision: Some(HookDecision::Block),
        reason: Some("blocked".into()),
        ..Default::default()
    };
    // continue=false 优先级高于 decision=block
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::PreventContinuation { .. }
    ));
}

#[test]
fn test_sync_response_decision_block() {
    let resp = SyncHookResponse {
        decision: Some(HookDecision::Block),
        reason: Some("blocked".into()),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::Block { reason } if reason == "blocked"
    ));
}

#[test]
fn test_sync_response_system_message() {
    let resp = SyncHookResponse {
        system_message: Some("msg".into()),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::SystemMessage { message } if message == "msg"
    ));
}

#[test]
fn test_sync_response_hook_specific_updated_input() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            updated_input: Some(serde_json::json!({"key": "val"})),
            permission_decision: None,
            permission_decision_reason: None,
            additional_context: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::ModifyInput { new_input } if new_input["key"] == "val"
    ));
}

#[test]
fn test_sync_response_hook_specific_permission_decision() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: Some(PermissionDecision::Deny),
            permission_decision_reason: Some("not allowed".into()),
            updated_input: None,
            additional_context: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::PermissionOverride { decision, .. } if decision == PermissionDecision::Deny
    ));
}

#[test]
fn test_sync_response_hook_specific_user_prompt_context() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::UserPromptSubmit {
            additional_context: Some("extra context".into()),
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::UserPromptSubmit),
        HookAction::AdditionalContext { context } if context == "extra context"
    ));
}

#[test]
fn test_sync_response_hook_specific_session_start_message() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::SessionStart {
            additional_context: None,
            initial_user_message: Some("start msg".into()),
            watch_paths: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::SessionStart),
        HookAction::InitialUserMessage { message } if message == "start msg"
    ));
}

#[test]
fn test_sync_response_default_allow() {
    let resp = SyncHookResponse::default();
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::Allow
    ));
}

#[test]
fn test_sync_response_decision_approve_is_allow() {
    let resp = SyncHookResponse {
        decision: Some(HookDecision::Approve),
        ..Default::default()
    };
    // Approve is not Block, so falls through to Allow
    assert!(matches!(
        sync_response_to_action(&resp, &HookEvent::PreToolUse),
        HookAction::Allow
    ));
}

// === H4：PreToolUse 组合输出不互吞 ===

#[test]
fn test_pretooluse_deny_with_updated_input_keeps_all_fields() {
    let resp = SyncHookResponse {
        system_message: Some("client hint".into()),
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: Some(PermissionDecision::Deny),
            permission_decision_reason: Some("hook reason".into()),
            updated_input: Some(serde_json::json!({"command": "safe-ls"})),
            additional_context: Some("extra ctx".into()),
        }),
        ..Default::default()
    };
    match sync_response_to_action(&resp, &HookEvent::PreToolUse) {
        HookAction::PermissionOverride {
            decision,
            reason,
            updated_input,
            additional_context,
            system_message,
        } => {
            assert_eq!(decision, PermissionDecision::Deny);
            assert_eq!(reason.as_deref(), Some("hook reason"));
            assert_eq!(
                updated_input.expect("updatedInput 不得被 deny 吞掉")["command"],
                "safe-ls"
            );
            assert_eq!(additional_context.as_deref(), Some("extra ctx"));
            assert_eq!(system_message.as_deref(), Some("client hint"));
        }
        other => panic!("expected combined PermissionOverride, got {other:?}"),
    }
}

#[test]
fn test_pretooluse_allow_with_updated_input_keeps_both() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: Some(PermissionDecision::Allow),
            permission_decision_reason: None,
            updated_input: Some(serde_json::json!({"command": "echo ok"})),
            additional_context: None,
        }),
        ..Default::default()
    };
    match sync_response_to_action(&resp, &HookEvent::PreToolUse) {
        HookAction::PermissionOverride {
            decision,
            updated_input,
            ..
        } => {
            assert_eq!(decision, PermissionDecision::Allow);
            assert_eq!(
                updated_input.expect("updatedInput 不得被 allow 吞掉")["command"],
                "echo ok"
            );
        }
        other => panic!("expected combined PermissionOverride, got {other:?}"),
    }
}

#[test]
fn test_pretooluse_additional_context_only_is_not_swallowed() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: None,
            permission_decision_reason: None,
            updated_input: None,
            additional_context: Some("ctx only".into()),
        }),
        ..Default::default()
    };
    match sync_response_to_action(&resp, &HookEvent::PreToolUse) {
        HookAction::PermissionOverride {
            decision,
            additional_context,
            updated_input,
            ..
        } => {
            assert_eq!(decision, PermissionDecision::Passthrough);
            assert_eq!(additional_context.as_deref(), Some("ctx only"));
            assert!(updated_input.is_none());
        }
        other => panic!("expected PermissionOverride carrying context, got {other:?}"),
    }
}

#[test]
fn test_pretooluse_invalid_decision_is_not_allow() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: Some(PermissionDecision::Invalid("explode".into())),
            permission_decision_reason: None,
            updated_input: None,
            additional_context: None,
        }),
        ..Default::default()
    };
    match sync_response_to_action(&resp, &HookEvent::PreToolUse) {
        HookAction::PermissionOverride { decision, .. } => {
            assert_eq!(decision, PermissionDecision::Invalid("explode".into()));
        }
        other => panic!("非法 decision 不得静默 Allow，got {other:?}"),
    }
}

#[test]
fn test_pretooluse_invalid_decision_in_raw_json_is_not_allow() {
    // 端到端：未知 permissionDecision 取值不得让整份输出 fail-open 成 Allow
    let action = parse_command_hook_output(
        &HookEvent::PreToolUse,
        r#"{"hook_specific_output":{"hookEventName":"PreToolUse","permissionDecision":"explode"}}"#,
    );
    match action {
        HookAction::PermissionOverride { decision, .. } => {
            assert_eq!(decision, PermissionDecision::Invalid("explode".into()));
        }
        other => panic!("非法 permissionDecision 不得静默 Allow，got {other:?}"),
    }
}
