//! `initialize` terminal auth 广告的唯一判定点回归。
//!
//! 三种合法标记（ACP v1 `clientCapabilities.auth.terminal`、Registry
//! `clientCapabilities._meta["terminal-auth"]`、顶层 `_meta["terminal-auth"]`）
//! 各产生恰好一个 `peri-setup` 方法；`clientCapabilities.terminal`
//! （terminal/* 工具能力）与缺失标记不得触发。

use serde_json::{json, Value};

use super::{build_auth_required_initialize_response, terminal_auth_methods};

fn assert_terminal_setup_method(params: Value) {
    let methods = terminal_auth_methods(&params);
    assert_eq!(methods.len(), 1, "应恰好广告一个认证方法: {params}");
    let value = serde_json::to_value(&methods[0]).expect("AuthMethod 序列化不应失败");
    assert_eq!(value["type"], "terminal", "认证方法类型: {value}");
    assert_eq!(value["id"], "peri-setup", "认证方法 ID: {value}");
    assert_eq!(value["name"], "Configure Peri", "认证方法名称: {value}");
    assert!(
        value["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "认证方法必须有描述: {value}"
    );
    assert_eq!(value["args"], json!(["--setup"]), "认证参数: {value}");
    assert_eq!(
        value["env"],
        json!({"PERI_ACP_SETUP": "1"}),
        "认证环境: {value}"
    );
}

fn assert_no_methods(params: Value) {
    assert!(
        terminal_auth_methods(&params).is_empty(),
        "未声明认证能力时不得广告方法: {params}"
    );
}

#[test]
fn test_v1_auth_terminal_advertises_setup_method() {
    assert_terminal_setup_method(json!({
        "protocolVersion": 1,
        "clientCapabilities": {"auth": {"terminal": true}},
    }));
}

#[test]
fn test_registry_client_meta_marker_advertises_setup_method() {
    assert_terminal_setup_method(json!({
        "protocolVersion": 1,
        "clientCapabilities": {"_meta": {"terminal-auth": true}},
    }));
}

#[test]
fn test_top_level_meta_marker_advertises_setup_method() {
    assert_terminal_setup_method(json!({
        "protocolVersion": 1,
        "_meta": {"terminal-auth": true},
    }));
}

#[test]
fn test_all_markers_together_still_advertise_exactly_one_method() {
    assert_terminal_setup_method(json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "auth": {"terminal": true},
            "_meta": {"terminal-auth": true},
        },
        "_meta": {"terminal-auth": true},
    }));
}

#[test]
fn test_terminal_tool_capability_is_not_an_auth_marker() {
    assert_no_methods(json!({
        "protocolVersion": 1,
        "clientCapabilities": {"terminal": true},
    }));
}

#[test]
fn test_missing_or_false_markers_advertise_nothing() {
    for params in [
        json!({"protocolVersion": 1}),
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
        json!({"protocolVersion": 1, "clientCapabilities": {"auth": {}}}),
        json!({"protocolVersion": 1, "clientCapabilities": {"auth": {"terminal": false}}}),
        json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {"terminal-auth": false}}}),
        json!({"protocolVersion": 1, "_meta": {"terminal-auth": "true"}}),
    ] {
        assert_no_methods(params);
    }
}

#[test]
fn test_auth_required_initialize_response_is_minimal_and_honest() {
    let response = build_auth_required_initialize_response(&json!({
        "protocolVersion": 1,
        "clientCapabilities": {"auth": {"terminal": true}},
    }));
    let value = serde_json::to_value(response).expect("InitializeResponse 序列化不应失败");
    assert_eq!(value["protocolVersion"], 1);
    assert_eq!(value["authMethods"][0]["id"], "peri-setup");
    assert_eq!(value["agentCapabilities"]["loadSession"], false);
    for key in ["list", "close", "resume", "fork", "delete"] {
        assert!(
            value["agentCapabilities"]["sessionCapabilities"]
                .get(key)
                .is_none(),
            "最小宿主不得声明 sessionCapabilities.{key}: {value}"
        );
    }
    assert_eq!(value["agentCapabilities"]["_meta"]["peri.replay"], false);
}
