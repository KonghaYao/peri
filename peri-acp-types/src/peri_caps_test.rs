use super::{default_ui_commands, PeriCaps};
use crate::command::command_route::UiCommandSpec;
use serde_json::json;

#[test]
fn test_default_all_false() {
    let caps = PeriCaps::default();
    assert!(!caps.token_stats);
    assert!(!caps.skill_names);
    assert!(!caps.replay);
    assert!(!caps.agent_event);
    assert!(!caps.agent_event_done);
    assert!(!caps.agent_activity);
    assert!(!caps.oauth);
    assert!(!caps.unstable_event);
    assert!(!caps.prediction);
    assert!(!caps.plan_entry_active_form);
    assert!(!caps.rewind);
    assert!(caps.ui_commands.is_empty());
}

#[test]
fn test_from_client_meta_all_true() {
    let meta = json!({
        "peri.tokenStats": true,
        "peri.skillNames": true,
        "peri.replay": true,
        "peri.agentEvent": true,
        "peri.agentEventDone": true,
        "peri.agentActivity": true,
        "peri.oauth": true,
        "peri.unstableEvent": true,
        "peri.prediction": true,
        "peri.planEntryActiveForm": true,
        "peri.rewind": true,
        "peri.hitlPending": true,
        "peri.contextUsage": true,
        "peri.sourceAgentId": true,
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(caps.skill_names);
    assert!(caps.replay);
    assert!(caps.agent_event);
    assert!(caps.agent_event_done);
    assert!(caps.agent_activity);
    assert!(caps.oauth);
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    assert!(caps.plan_entry_active_form);
    assert!(caps.rewind);
    // 已删除的 cap key（peri.hitlPending / peri.contextUsage / peri.sourceAgentId）
    // 不再解析，声明也不生效——仅保留 uiCommands 未声明默认空语义
    // meta 未声明 peri.uiCommands → 默认空（不广播 ui 条目）
    assert!(caps.ui_commands.is_empty());
}

#[test]
fn test_from_client_meta_partial() {
    let meta = json!({
        "peri.tokenStats": true,
        "peri.replay": true,
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(caps.replay);
    assert!(!caps.skill_names);
}

#[test]
fn test_from_client_meta_empty() {
    let empty = serde_json::Map::new();
    let caps = PeriCaps::from_client_meta(&empty);
    assert_eq!(caps, PeriCaps::default());
}

#[test]
fn test_from_client_meta_unknown_keys_ignored() {
    let meta = json!({
        "peri.tokenStats": true,
        "some.unknown": "ignored",
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(!caps.replay);
}

#[test]
fn test_to_agent_meta_roundtrip() {
    let caps = PeriCaps {
        token_stats: true,
        skill_names: false,
        replay: true,
        ..Default::default()
    };
    let meta = caps.to_agent_meta();
    let caps2 = PeriCaps::from_client_meta(&meta);
    assert_eq!(caps, caps2);
}

#[test]
fn test_all_enabled() {
    let caps = PeriCaps::all_enabled();
    assert!(caps.token_stats);
    assert!(caps.skill_names);
    assert!(caps.replay);
    assert!(caps.agent_event);
    assert!(caps.agent_event_done);
    assert!(caps.agent_activity);
    assert!(caps.oauth);
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    assert!(caps.plan_entry_active_form);
    assert!(caps.rewind);
    // ui_commands 填默认 11 条明细（数据迁移自 dispatch/commands.rs UI_COMMANDS）
    assert_eq!(caps.ui_commands, default_ui_commands());
    assert_eq!(caps.ui_commands.len(), 11);
    let names: Vec<&str> = caps.ui_commands.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "help", "clear", "context", "cost", "mode", "effort", "history", "agents", "rename",
            "lang", "exit",
        ]
    );
    // 默认明细均无参数、无别名
    assert!(caps.ui_commands.iter().all(|s| s.aliases.is_empty()));
    assert!(caps.ui_commands.iter().all(|s| s.args.is_none()));
}

#[test]
fn test_ui_commands_roundtrip() {
    // 未声明 → 空（外部客户端默认不接收界面性命令）
    let caps = PeriCaps::from_client_meta(&serde_json::json!({}).as_object().unwrap().clone());
    assert!(caps.ui_commands.is_empty());
    // 旧客户端 bool true → 退化为默认 11 条明细（等价现状行为）
    let meta = serde_json::json!({ "peri.uiCommands": true });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert_eq!(caps.ui_commands, default_ui_commands());
    // bool false → 空
    let meta = serde_json::json!({ "peri.uiCommands": false });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.ui_commands.is_empty());
    // 数组 → 解析为明细；回显序列化为数组，往返一致
    let specs = vec![
        UiCommandSpec {
            name: "history".into(),
            aliases: vec!["h".into(), "hist".into()],
            description: "查看历史".into(),
            args: None,
        },
        UiCommandSpec {
            name: "cost".into(),
            description: "Show token usage".into(),
            ..Default::default()
        },
    ];
    let meta = serde_json::json!({ "peri.uiCommands": specs });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert_eq!(caps.ui_commands, specs);
    let echo = caps.to_agent_meta();
    assert!(
        echo.get("peri.uiCommands")
            .is_some_and(serde_json::Value::is_array),
        "回显的 peri.uiCommands 应为数组"
    );
    let caps2 = PeriCaps::from_client_meta(&echo);
    assert_eq!(caps2, caps);
    // 数组元素缺省字段补默认（name 必填，其余 serde default 兜底）
    let meta = serde_json::json!({ "peri.uiCommands": [{ "name": "lang" }] });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert_eq!(caps.ui_commands.len(), 1);
    assert_eq!(caps.ui_commands[0].name, "lang");
    assert!(caps.ui_commands[0].aliases.is_empty());
    assert_eq!(caps.ui_commands[0].description, "");
    assert_eq!(caps.ui_commands[0].args, None);
    // 非法数组（元素类型不合法）→ 解析失败按空处理
    let meta = serde_json::json!({ "peri.uiCommands": [123] });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.ui_commands.is_empty());
    // 其他类型 → 按空处理
    let meta = serde_json::json!({ "peri.uiCommands": "help" });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.ui_commands.is_empty());
}
