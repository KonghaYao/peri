use super::*;

#[test]
fn test_hooktype_deser_command() {
    let json = r#"{"type": "command", "command": "echo test"}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    match hook {
        HookType::Command { command, .. } => assert_eq!(command, "echo test"),
        _ => panic!("Expected Command variant"),
    }
}

#[test]
fn test_hooktype_deser_prompt() {
    let json = r#"{"type": "prompt", "prompt": "analyze this"}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    match hook {
        HookType::Prompt { prompt, .. } => assert_eq!(prompt, "analyze this"),
        _ => panic!("Expected Prompt variant"),
    }
}

#[test]
fn test_hooktype_deser_http() {
    let json = r#"{"type": "http", "url": "https://example.com/hook"}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    match hook {
        HookType::Http { url, .. } => assert_eq!(url, "https://example.com/hook"),
        _ => panic!("Expected Http variant"),
    }
}

#[test]
fn test_hooktype_deser_agent() {
    let json = r#"{"type": "agent", "prompt": "review this code"}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    match hook {
        HookType::Agent { prompt, .. } => assert_eq!(prompt, "review this code"),
        _ => panic!("Expected Agent variant"),
    }
}

#[test]
fn test_hooktype_deser_with_condition() {
    let json = r#"{"type": "command", "command": "echo check", "if": "Bash(git commit)"}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    assert_eq!(hook.get_condition(), Some(&"Bash(git commit)".to_string()));
}

#[test]
fn test_hooktype_deser_async_field() {
    let json = r#"{"type": "command", "command": "echo async", "async": true}"#;
    let hook: HookType = serde_json::from_str(json).unwrap();
    assert!(hook.is_async());
}

#[test]
fn test_hookevent_serialize() {
    let event = HookEvent::PreToolUse;
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(json, "\"PreToolUse\"");
}

#[test]
fn test_hookevent_deserialize() {
    let event: HookEvent = serde_json::from_str("\"PreToolUse\"").unwrap();
    assert_eq!(event, HookEvent::PreToolUse);
}

#[test]
fn test_hookinput_serialize_tool_call() {
    let input = HookInput::tool_call(
        "sess-123",
        "/tmp/transcript.json",
        "/project",
        "yolo",
        "Bash",
        &serde_json::json!({"command": "ls"}),
        "call-456",
    );
    let json = serde_json::to_string(&input).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["hook_event_name"], "PreToolUse");
    assert_eq!(parsed["tool_name"], "Bash");
    assert_eq!(parsed["session_id"], "sess-123");
    assert_eq!(parsed["cwd"], "/project");
    // None fields should be skipped
    assert!(parsed.get("prompt").is_none());
    assert!(parsed.get("source").is_none());
}

#[test]
fn test_sync_hook_response_deser_continue_false() {
    let json = r#"{"continue": false, "stopReason": "blocked by hook"}"#;
    let resp: SyncHookResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.continue_run, Some(false));
    assert_eq!(resp.stop_reason.as_deref(), Some("blocked by hook"));
}

#[test]
fn test_sync_hook_response_deser_decision_block() {
    let json = r#"{"decision": "block", "reason": "not allowed"}"#;
    let resp: SyncHookResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.decision, Some(HookDecision::Block));
    assert_eq!(resp.reason.as_deref(), Some("not allowed"));
}

#[test]
fn test_hook_specific_output_pre_tool_use() {
    let json = r#"{"hookEventName": "PreToolUse", "permissionDecision": "deny"}"#;
    let output: HookSpecificOutput = serde_json::from_str(json).unwrap();
    match output {
        HookSpecificOutput::PreToolUse {
            permission_decision,
            ..
        } => assert_eq!(permission_decision, Some(PermissionDecision::Deny)),
        _ => panic!("Expected PreToolUse variant"),
    }
}

#[test]
fn test_hook_specific_output_session_start() {
    let json = r#"{"hookEventName": "SessionStart", "additionalContext": "extra info", "initialUserMessage": "start msg"}"#;
    let output: HookSpecificOutput = serde_json::from_str(json).unwrap();
    match output {
        HookSpecificOutput::SessionStart {
            additional_context,
            initial_user_message,
            ..
        } => {
            assert_eq!(additional_context.as_deref(), Some("extra info"));
            assert_eq!(initial_user_message.as_deref(), Some("start msg"));
        }
        _ => panic!("Expected SessionStart variant"),
    }
}

#[test]
fn test_hooktype_getter_methods() {
    let hook: HookType = serde_json::from_str(
        r#"{"type": "command", "command": "echo", "once": true, "matcher": "Bash"}"#,
    )
    .unwrap();
    assert_eq!(hook.get_matcher(), Some(&"Bash".to_string()));
    assert!(hook.is_once());
    assert!(!hook.is_async());
}

#[test]
fn test_hookinput_constructors() {
    let input = HookInput::session_start("s1", "/t.json", "/p", "startup", "opus");
    assert_eq!(input.hook_event_name, HookEvent::SessionStart);
    assert_eq!(input.source.as_deref(), Some("startup"));

    let input2 = HookInput::tool_call(
        "s1",
        "/t.json",
        "/p",
        "yolo",
        "Write",
        &serde_json::json!({}),
        "c1",
    );
    assert_eq!(input2.hook_event_name, HookEvent::PreToolUse);
    assert_eq!(input2.tool_name.as_deref(), Some("Write"));

    let input3 = HookInput::tool_result(
        "s1",
        "/t.json",
        "/p",
        "yolo",
        "Bash",
        &serde_json::json!({}),
        &serde_json::json!({"out": "ok"}),
        false,
    );
    assert_eq!(input3.hook_event_name, HookEvent::PostToolUse);

    let input4 = HookInput::tool_result(
        "s1",
        "/t.json",
        "/p",
        "yolo",
        "Bash",
        &serde_json::json!({}),
        &serde_json::json!({"err": "fail"}),
        true,
    );
    assert_eq!(input4.hook_event_name, HookEvent::PostToolUseFailure);

    let input5 = HookInput::user_prompt_submit("s1", "/t.json", "/p", "hello");
    assert_eq!(input5.hook_event_name, HookEvent::UserPromptSubmit);
    assert_eq!(input5.prompt.as_deref(), Some("hello"));

    let input6 = HookInput::subagent_start("s1", "/t.json", "/p", "reviewer");
    assert_eq!(input6.hook_event_name, HookEvent::SubagentStart);

    let input7 = HookInput::subagent_stop("s1", "/t.json", "/p", "reviewer", "done");
    assert_eq!(input7.hook_event_name, HookEvent::SubagentStop);
    assert_eq!(input7.subagent_result.as_deref(), Some("done"));
}

#[test]
fn test_hooks_config_deser() {
    let json = r#"{
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "echo checking bash"}]
                }
            ]
        }"#;
    let config: HooksConfig = serde_json::from_str(json).unwrap();
    assert!(config.contains_key(&HookEvent::PreToolUse));
    let rules = &config[&HookEvent::PreToolUse];
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].matcher.as_deref(), Some("Bash"));
    assert_eq!(rules[0].hooks.len(), 1);
}

#[test]
fn test_permission_decision_deser() {
    let d: PermissionDecision = serde_json::from_str("\"deny\"").unwrap();
    assert_eq!(d, PermissionDecision::Deny);
    let d2: PermissionDecision = serde_json::from_str("\"allow\"").unwrap();
    assert_eq!(d2, PermissionDecision::Allow);
    let d3: PermissionDecision = serde_json::from_str("\"ask\"").unwrap();
    assert_eq!(d3, PermissionDecision::Ask);
    let d4: PermissionDecision = serde_json::from_str("\"passthrough\"").unwrap();
    assert_eq!(d4, PermissionDecision::Passthrough);
}

#[test]
fn test_post_tool_batch_serialization() {
    let json = "\"PostToolBatch\"";
    let event: HookEvent = serde_json::from_str(json).unwrap();
    assert_eq!(event, HookEvent::PostToolBatch);
    let back = serde_json::to_string(&event).unwrap();
    assert_eq!(back, json);
}
