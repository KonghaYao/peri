use super::*;
use crate::kit::acp_types::CacheUsageSample;
use crate::kit::slash_completion::SlashActionKind;
use crate::kit::slash_projection::ArgKind;
use serde_json::json;
use serial_test::serial;
#[test]
#[serial]
fn available_commands_update_refreshes_mcp_slash_cache_immediately() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    crate::kit::input_area::refresh_slash_items();

    let payload = json!({
        "sessionId": "s-mcp",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [{
                "name": "demo:hello",
                "description": "MCP skill hello",
                "_meta": {"periKind": "mcp_skill", "periLevel": 2}
            }]
        }
    });
    let (bridge_tx, _bridge_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &bridge_tx, "test");

    let items = crate::kit::input_area::get_cached_slash_items();
    assert!(
        items.iter().any(|item| item.insert_text == "demo:hello"),
        "MCP skill 应在 available_commands_update 后立即出现在 slash 缓存"
    );
}

/// 验证 handle_session_update 能正确解析 ACP SessionUpdate 的 JSON 格式。
/// SessionUpdate 使用 #[serde(tag = "sessionUpdate")] 内部标签，字段名 camelCase。
#[test]
#[serial]
fn test_handle_session_update_parses_available_commands() {
    crate::kit::atoms::init_atoms();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {"name": "help", "description": "Show help"},
                {"name": "clear", "description": "Clear conversation"},
                {"name": "archify", "description": "Create architecture diagrams"}
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 3);
    // Phase 4 步骤 1：投影 DTO 结构化后按字段断言；缺 _meta 的条目
    // 回退 kind=Command / level=1（步骤 2 升级解析后再断言元数据字段）。
    assert_eq!(entries[0].fullname, "help");
    assert_eq!(entries[0].description, "Show help");
    assert_eq!(entries[0].kind, SlashActionKind::Command);
    assert_eq!(entries[0].level, 1);
    assert_eq!(entries[2].fullname, "archify");
    assert_eq!(entries[2].description, "Create architecture diagrams");
}

/// Phase 4 步骤 2：投影 _meta 全字段解析——periKind / periLevel /
/// periAliases / periCategory / periArgs（含 flags 为 **object 数组的
/// `FlagSpec` 往返**——wire 与本地镜像对齐，P1-5）。
#[test]
#[serial]
fn test_handle_session_update_parses_projection_fields() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {
                    "name": "demo:hello",
                    "description": "MCP skill hello",
                    "_meta": {
                        "periKind": "mcp_skill",
                        "periLevel": 2,
                        "periAliases": ["h", "hello"],
                        "periCategory": "mcp",
                        "periArgs": {
                            "positionals": [
                                {"name": "file", "kind": "Path", "required": true}
                            ],
                            "named": [
                                {"name": "out", "kind": "String", "required": false}
                            ],
                            "flags": [
                                {"name": "force", "short": "-f", "description": "force it"}
                            ]
                        }
                    }
                }
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.fullname, "demo:hello");
    assert_eq!(e.description, "MCP skill hello");
    assert_eq!(e.kind, SlashActionKind::McpSkill);
    assert_eq!(e.level, 2);
    assert_eq!(e.aliases, vec!["h".to_string(), "hello".to_string()]);
    assert_eq!(e.category.as_deref(), Some("mcp"));
    // ArgsSchema 全字段往返（flags 为 object 数组的 FlagSpec）
    let args = e.args.as_ref().expect("periArgs 应解析为 ArgsSchema");
    assert_eq!(args.positionals.len(), 1);
    assert_eq!(args.positionals[0].name, "file");
    assert_eq!(args.positionals[0].kind, ArgKind::Path);
    assert!(args.positionals[0].required);
    assert_eq!(args.named.len(), 1);
    assert_eq!(args.named[0].name, "out");
    assert_eq!(args.named[0].kind, ArgKind::String);
    assert!(!args.named[0].required);
    assert_eq!(args.flags.len(), 1);
    assert_eq!(args.flags[0].name, "force");
    assert_eq!(args.flags[0].short.as_deref(), Some("-f"));
    assert_eq!(args.flags[0].description.as_deref(), Some("force it"));
}

/// Phase 4 步骤 2：缺 _meta 元数据的投影条目回退 kind=Command / level=1 /
/// args=None / aliases=[]（R1：条目缺 kind 时分类整体退化 Command）。
#[test]
#[serial]
fn test_handle_session_update_projection_missing_meta_defaults() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {"name": "plaincmd", "description": "普通命令"},
                {
                    "name": "weird:entry",
                    "description": "未知 kind / 非法 level",
                    "_meta": {
                        "periKind": "unknown_kind",
                        "periLevel": 9
                    }
                }
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 2);
    for e in &entries {
        assert_eq!(
            e.kind,
            SlashActionKind::Command,
            "未知/缺失 kind 回退 Command"
        );
        assert_eq!(e.level, 1, "缺失/非法 level 回退 1");
        assert!(e.args.is_none(), "缺失 periArgs → args=None");
        assert!(e.aliases.is_empty(), "缺失 periAliases → aliases=[]");
        assert!(e.category.is_none(), "缺失 periCategory → category=None");
    }
}

/// 验证非 available_commands_update 的 session/update 不会错误写入 atom。
#[test]
#[serial]
fn test_handle_session_update_skips_non_command_update() {
    crate::kit::atoms::init_atoms();
    // 重置 atom 状态，避免跨测试污染
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "usage_update",
            "used": 1000,
            "total": 200000
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 0, "非 commands update 不应写入 atom");
}

/// usage_update 只解码为 bridge-owned sample；notifier 不再逐步推送 warning。
#[test]
#[serial]
fn test_usage_update_decodes_root_cache_sample_without_per_step_notification() {
    crate::kit::atoms::init_atoms();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "usage_update",
            "_meta": {
                "inputTokens": 20000,
                "outputTokens": 100,
                "cacheReadTokens": 2000,
                "requestId": "req-1"
            }
        }
    });
    let (dummy_tx, mut dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = handle_session_update(payload, &dummy_tx, "test");
    match result {
        Some(AcpEventData::CacheUsageUpdated(Some(sample))) => {
            assert_eq!(sample.input_tokens, 20_000);
            assert_eq!(sample.cached_tokens, 2_000);
            assert_eq!(sample.request_id.as_deref(), Some("req-1"));
        }
        other => panic!("expected cache usage sample, got {other:?}"),
    }
    assert!(
        dummy_rx.try_recv().is_err(),
        "notifier must not push a per-step cache warning"
    );
}

#[test]
fn test_usage_update_ignores_auxiliary_and_preserves_explicit_zero_cache_read() {
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let payload = |usage_meta: serde_json::Value, params_meta: serde_json::Value| {
        json!({
            "_meta": params_meta,
            "update": {
                "sessionUpdate": "usage_update",
                "_meta": usage_meta
            }
        })
    };
    assert!(
        handle_session_update(
            payload(
                json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 70}),
                json!({"peri": {"sourceAgentId": "child"}})
            ),
            &dummy_tx,
            "test"
        )
        .is_none()
    );
    let zero = handle_session_update(
        payload(
            json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 0}),
            json!({}),
        ),
        &dummy_tx,
        "test",
    );
    assert!(matches!(
        zero,
        Some(AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
            input_tokens: 100,
            cached_tokens: 0,
            ..
        })))
    ));
    let invalid_meta = json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 101});
    assert!(
        matches!(
            handle_session_update(payload(invalid_meta, json!({})), &dummy_tx, "test"),
            Some(AcpEventData::CacheUsageUpdated(None))
        ),
        "inconsistent root usage must remain unavailable"
    );
    assert!(
        handle_session_update(
            payload(json!({"inputTokens": 100, "outputTokens": 1}), json!({})),
            &dummy_tx,
            "test"
        )
        .is_none(),
        "missing cacheReadTokens must not clear a prior root sample"
    );
}

/// 验证 handle_session_update 能正确解析 plan update 并写入 TODO_ITEMS atom。
#[test]
#[serial]
fn test_handle_session_update_parses_plan() {
    use crate::kit::message_area::TodoStatus;
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "plan",
            "entries": [
                {"content": "Fix bug", "status": "in_progress", "priority": "medium"},
                {"content": "Write tests", "status": "pending", "priority": "medium"},
                {"content": "Document", "status": "completed", "priority": "medium"}
            ]
        }
    });

    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");

    let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
    assert_eq!(items.len(), 3, "应包含 3 个条目，实际: {items:?}");
    assert_eq!(items[0].content, "Fix bug");
    assert_eq!(items[1].content, "Write tests");
    assert_eq!(items[2].content, "Document");
    assert!(matches!(items[0].status, TodoStatus::InProgress));
    assert!(matches!(items[1].status, TodoStatus::Pending));
    assert!(matches!(items[2].status, TodoStatus::Completed));
}
