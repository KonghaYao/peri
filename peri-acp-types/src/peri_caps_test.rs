use super::PeriCaps;
use serde_json::json;

#[test]
fn test_default_all_false() {
    let caps = PeriCaps::default();
    assert!(!caps.token_stats);
    assert!(!caps.skill_names);
    assert!(!caps.replay);
    assert!(!caps.agent_event);
    assert!(!caps.agent_event_done);
    assert!(!caps.unstable_event);
    assert!(!caps.prediction);
}

#[test]
fn test_from_client_meta_all_true() {
    let meta = json!({
        "peri.tokenStats": true,
        "peri.skillNames": true,
        "peri.replay": true,
        "peri.agentEvent": true,
        "peri.agentEventDone": true,
        "peri.unstableEvent": true,
        "peri.prediction": true,
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
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    // 已删除的 cap key（peri.hitlPending / peri.contextUsage / peri.sourceAgentId）
    // 不再解析，声明也不生效——仅保留 uiCommands 未声明默认 false 语义
    // meta 未声明 peri.uiCommands → 默认 false
    assert!(!caps.ui_commands);
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
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    assert!(caps.ui_commands);
}

#[test]
fn test_ui_commands_roundtrip() {
    // 未声明 → false（外部客户端默认不接收界面性命令）
    let caps = PeriCaps::from_client_meta(&serde_json::json!({}).as_object().unwrap().clone());
    assert!(!caps.ui_commands);
    // 声明 peri.uiCommands → true；回显序列化保留
    let meta = serde_json::json!({ "peri.uiCommands": true });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.ui_commands);
    let echo = caps.to_agent_meta();
    assert_eq!(
        echo.get("peri.uiCommands"),
        Some(&serde_json::Value::Bool(true))
    );
}
