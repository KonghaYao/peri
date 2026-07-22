use std::collections::HashMap;

use tempfile::tempdir;

use super::*;
use crate::hooks::types::HookEvent;
use crate::hooks::types::HookType;

fn make_manifest_with_hooks(hooks: Option<HooksConfig>) -> PluginManifest {
    PluginManifest {
        name: "test-plugin".into(),
        version: "1.0.0".into(),
        description: String::new(),
        author: None,
        commands: None,
        agents: None,
        skills: None,
        hooks,
        mcp_servers: None,
        lsp_servers: None,
        output_styles: None,
        channels: None,
        options: None,
        settings: None,
        extra: serde_json::json!({}),
    }
}

#[test]
fn test_file_priority_over_manifest() {
    let dir = tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    // File has PreToolUse
    let file_config = r#"{
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "echo file-hook"}]
                }
            ]
        }"#;
    std::fs::write(hooks_dir.join("hooks.json"), file_config).unwrap();

    // Manifest has PostToolUse
    let mut manifest_hooks: HooksConfig = HashMap::new();
    manifest_hooks.insert(crate::hooks::types::HookEvent::PostToolUse, vec![]);
    let manifest = make_manifest_with_hooks(Some(manifest_hooks));

    let result = extract_hooks(&manifest, dir.path()).unwrap();
    assert!(result.contains_key(&crate::hooks::types::HookEvent::PreToolUse));
    assert!(!result.contains_key(&crate::hooks::types::HookEvent::PostToolUse));
}

#[test]
fn test_fallback_to_manifest_hooks() {
    let dir = tempdir().unwrap();
    // No hooks/hooks.json file

    let mut manifest_hooks: HooksConfig = HashMap::new();
    manifest_hooks.insert(crate::hooks::types::HookEvent::SessionStart, vec![]);
    let manifest = make_manifest_with_hooks(Some(manifest_hooks));

    let result = extract_hooks(&manifest, dir.path()).unwrap();
    assert!(result.contains_key(&crate::hooks::types::HookEvent::SessionStart));
}

#[test]
fn test_both_missing_returns_none() {
    let dir = tempdir().unwrap();
    let manifest = make_manifest_with_hooks(None);

    let result = extract_hooks(&manifest, dir.path());
    assert!(result.is_none());
}

#[test]
fn test_invalid_json_falls_back_to_manifest() {
    let dir = tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    // Invalid JSON in hooks.json
    std::fs::write(hooks_dir.join("hooks.json"), "not valid json").unwrap();

    let mut manifest_hooks: HooksConfig = HashMap::new();
    manifest_hooks.insert(crate::hooks::types::HookEvent::Stop, vec![]);
    let manifest = make_manifest_with_hooks(Some(manifest_hooks));

    // Should fall back to manifest hooks
    let result = extract_hooks(&manifest, dir.path()).unwrap();
    assert!(result.contains_key(&crate::hooks::types::HookEvent::Stop));
}

#[test]
fn test_empty_hooks_returns_empty_hashmap() {
    let dir = tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(hooks_dir.join("hooks.json"), "{}").unwrap();

    let manifest = make_manifest_with_hooks(None);
    let result = extract_hooks(&manifest, dir.path()).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_load_settings_local_hooks_basic() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo pre"}
                    ]
                }
            ],
            "Notification": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo notify"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_local_hooks(dir.path().to_str().unwrap());
    assert_eq!(hooks.len(), 2);

    // Verify plugin source
    for h in &hooks {
        assert_eq!(h.plugin_name, "settings.local.json");
    }

    // Check both events are present (order not guaranteed)
    let has_pre = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::PreToolUse));
    let has_notification = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::Notification));
    assert!(has_pre, "should have PreToolUse hook");
    assert!(has_notification, "should have Notification hook");
}

#[test]
fn test_load_settings_local_hooks_no_file() {
    let hooks = load_settings_local_hooks("/nonexistent/path");
    assert!(hooks.is_empty());
}

#[test]
fn test_load_settings_local_hooks_no_hooks_field() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.local.json"), "{}").unwrap();

    let hooks = load_settings_local_hooks(dir.path().to_str().unwrap());
    assert!(hooks.is_empty());
}

#[test]
fn test_load_settings_local_hooks_with_matcher() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PermissionRequest": [
                {
                    "matcher": ".env|.env.local",
                    "hooks": [
                        {"type": "command", "command": "echo changed"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_local_hooks(dir.path().to_str().unwrap());
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].matcher.as_deref(), Some(".env|.env.local"));
}

#[test]
fn test_load_from_real_project_dir() {
    // Test loading from the actual peri project directory
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let settings_path = std::path::Path::new(&cwd)
        .join(".claude")
        .join("settings.local.json");
    if !settings_path.exists() {
        eprintln!(
            "Skipping: no settings.local.json at {}",
            settings_path.display()
        );
        return;
    }
    let hooks = load_settings_local_hooks(&cwd);
    assert!(
        !hooks.is_empty(),
        "Should load hooks from project settings.local.json"
    );
    // Should have hooks for known events
    let has_pre = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::PreToolUse));
    let has_perm = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::PermissionRequest));
    assert!(has_pre, "Should have PreToolUse hook");
    assert!(has_perm, "Should have PermissionRequest hook");
}

#[test]
#[ignore = "需要 ~/.claude/settings.json 真实文件，CI 环境不存在"]
fn test_load_global_settings_hooks_real_file() {
    // 读取真实 ~/.claude/settings.json 并验证 hooks 解析
    let settings_path = dirs_next::home_dir()
        .expect("Cannot determine home directory")
        .join(".claude")
        .join("settings.json");
    assert!(
        settings_path.exists(),
        "settings.json not found at {}",
        settings_path.display()
    );

    let hooks = load_global_settings_hooks();

    // 预期 6 个事件，每个事件 1 个 command hook
    assert_eq!(
        hooks.len(),
        6,
        "Expected 6 hooks (6 events x 1 command), got {}",
        hooks.len()
    );

    // 验证所有期望的事件都存在
    let expected_events = [
        HookEvent::PermissionRequest,
        HookEvent::PreToolUse,
        HookEvent::SessionEnd,
        HookEvent::SessionStart,
        HookEvent::Stop,
        HookEvent::UserPromptSubmit,
    ];
    for expected_event in &expected_events {
        let found = hooks.iter().any(|h| &h.event == expected_event);
        assert!(found, "Missing hook for event {:?}", expected_event);
    }

    // 验证每个 hook 的字段
    for hook in &hooks {
        assert_eq!(
            hook.plugin_name, "settings.json",
            "plugin_name should be 'settings.json' for event {:?}",
            hook.event
        );
        assert_eq!(
            hook.plugin_id, "settings.global",
            "plugin_id should be 'settings.global'"
        );
        // 验证是 Command 类型，且命令包含 herdr-agent-state.sh
        match &hook.hook {
            HookType::Command { command, .. } => {
                assert!(
                    command.contains("herdr-agent-state.sh"),
                    "Command should contain herdr-agent-state.sh, got: {}",
                    command
                );
            }
            other => panic!("Expected Command hook, got {:?}", other),
        }
    }
}

// ===== load_settings_project_hooks 测试 =====

#[test]
fn test_load_settings_project_hooks_basic() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo pre"}
                    ]
                }
            ],
            "Notification": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo notify"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert_eq!(hooks.len(), 2);

    // 验证插件来源标识
    for h in &hooks {
        assert_eq!(h.plugin_name, "project-settings.json");
    }

    // 验证两个事件都存在（顺序不保证）
    let has_pre = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::PreToolUse));
    let has_notification = hooks
        .iter()
        .any(|h| matches!(&h.event, HookEvent::Notification));
    assert!(has_pre, "should have PreToolUse hook");
    assert!(has_notification, "should have Notification hook");
}

#[test]
fn test_load_settings_project_hooks_no_file() {
    let hooks = load_settings_project_hooks("/nonexistent/path");
    assert!(hooks.is_empty());
}

#[test]
fn test_load_settings_project_hooks_no_hooks_field() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"), "{}").unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert!(hooks.is_empty());
}

#[test]
fn test_load_settings_project_hooks_with_matcher() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PermissionRequest": [
                {
                    "matcher": ".env|.env.local",
                    "hooks": [
                        {"type": "command", "command": "echo changed"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].matcher.as_deref(), Some(".env|.env.local"));
}

// ===== 宽松解析测试 (P0-2) =====

#[test]
fn test_tolerant_mixed_valid_and_invalid_events() {
    // 场景：部分事件有效、部分无效，有效的事件应保留
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo valid"}
                    ]
                }
            ],
            "UnknownEvent": [  // 未知事件，应被跳过
                {
                    "hooks": [
                        {"type": "command", "command": "echo unknown"}
                    ]
                }
            ],
            "Notification": "not-an-array"  // 值不是数组，应被跳过
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    // 只有 PreToolUse 有效
    assert_eq!(hooks.len(), 1);
    assert!(matches!(&hooks[0].event, HookEvent::PreToolUse));
}

#[test]
fn test_tolerant_unknown_event_skipped() {
    // 场景：hooks 中所有 key 都是未知事件，应返回空
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "NonExistentEvent1": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo changed"}
                    ]
                }
            ],
            "NonExistentEvent2": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo setup"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert!(hooks.is_empty(), "unknown events should be skipped");
}

#[test]
fn test_tolerant_non_array_rules_skipped() {
    // 场景：事件 key 已知，但值不是数组（如字符串），应跳过该事件
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": "this-is-not-an-array",
            "Notification": [
                {
                    "hooks": [
                        {"type": "command", "command": "echo valid"}
                    ]
                }
            ]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    // 只有 Notification 有效
    assert_eq!(hooks.len(), 1);
    assert!(matches!(&hooks[0].event, HookEvent::Notification));
}

#[test]
fn test_tolerant_all_invalid_returns_empty() {
    // 场景：所有事件的 rules 格式都错误，应返回空列表（不 panic）
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": 42,
            "PostToolUse": null,
            "Notification": true
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert!(hooks.is_empty());
}

#[test]
fn test_tolerant_hooks_not_object_returns_empty() {
    // 场景：hooks 字段不是 object（如数组），应返回空
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let settings = serde_json::json!({
        "hooks": ["this-is-an-array-not-object"]
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let hooks = load_settings_project_hooks(dir.path().to_str().unwrap());
    assert!(hooks.is_empty());
}
