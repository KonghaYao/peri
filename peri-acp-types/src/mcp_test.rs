//! mcp.rs 契约测试：McpSubscriptionPort::downcast_arc 还原 + subscriptions 配置 serde。
//!
//! downcast 回归背景（同构 issue 2026-08-07-cron-tool-task-never-triggers，
//! 见 peri-middlewares/src/cron/mod_test.rs）：直接对 trait object 调 `type_id()`
//! 会解析到 trait object 自身（恒不等于具体类型）→ downcast 恒失败；必须经
//! `as_any()` 取具体类型 TypeId。

use std::{any::Any, sync::Arc};

use crate::mcp::McpSubscriptionPort;
use crate::plugin::{McpServerConfig, McpSubscriptionsConfig};
use crate::session::InboxHandle;

// ── 测试用具体端口实现 ───────────────────────────────────────────────────────

/// 记录注册/注销 session_id 的 fake 端口（downcast 还原目标）。
#[derive(Default)]
struct FakeSubscriptionPort {
    registered: std::sync::Mutex<Vec<String>>,
    unregistered: std::sync::Mutex<Vec<String>>,
}

impl McpSubscriptionPort for FakeSubscriptionPort {
    fn register_inbox(&self, session_id: &str, _handle: InboxHandle) {
        self.registered.lock().unwrap().push(session_id.to_string());
    }

    fn unregister_inbox(&self, session_id: &str) {
        self.unregistered
            .lock()
            .unwrap()
            .push(session_id.to_string());
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// 另一个具体实现（downcast 失败路径用：类型不匹配时须返回原 Arc 而非 panic）。
struct FakeOtherPort;

impl McpSubscriptionPort for FakeOtherPort {
    fn register_inbox(&self, _session_id: &str, _handle: InboxHandle) {}

    fn unregister_inbox(&self, _session_id: &str) {}

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── downcast_arc ─────────────────────────────────────────────────────────────

/// [回归测试] McpSubscriptionPort::downcast_arc 必须还原具体实例
/// （同构 issue 2026-08-07-cron-tool-task-never-triggers，装配面依赖
/// downcast 还原共享同一端口实例）。
#[test]
fn test_mcp_subscription_port_downcast_restores_concrete() {
    let concrete = Arc::new(FakeSubscriptionPort::default());
    let port: Arc<dyn McpSubscriptionPort> = concrete.clone() as Arc<dyn McpSubscriptionPort>;

    let restored = match Arc::clone(&port).downcast_arc::<FakeSubscriptionPort>() {
        Ok(p) => p,
        Err(_) => panic!("downcast 必须还原具体类型 FakeSubscriptionPort"),
    };
    assert!(
        Arc::ptr_eq(&concrete, &restored),
        "还原实例必须是原 Arc（SessionManager 与实现侧共享同一端口）"
    );
}

/// downcast 类型不匹配：Err 分支返回原 Arc，不得 panic 或悬垂指针。
#[test]
fn test_mcp_subscription_port_downcast_wrong_type_returns_original() {
    let port: Arc<dyn McpSubscriptionPort> = Arc::new(FakeSubscriptionPort::default());
    let err = match Arc::clone(&port).downcast_arc::<FakeOtherPort>() {
        Ok(_) => panic!("类型不匹配时 downcast 必须失败"),
        Err(p) => p,
    };
    assert!(Arc::ptr_eq(&port, &err), "失败分支必须返回原 Arc");
}

// ── subscriptions 配置 serde ─────────────────────────────────────────────────

/// McpSubscriptionsConfig serde round-trip：camelCase 字段名往返一致。
#[test]
fn test_mcp_subscriptions_config_camel_case_roundtrip() {
    let sub = McpSubscriptionsConfig {
        resources: vec!["file:///notes/1.md".to_string()],
        tools_list_changed: true,
        prompts_list_changed: false,
        resources_list_changed: true,
    };
    let json = serde_json::to_string(&sub).unwrap();
    assert!(
        json.contains("\"toolsListChanged\":true"),
        "camelCase 字段名: {json}"
    );
    assert!(
        json.contains("\"resourcesListChanged\":true"),
        "camelCase 字段名: {json}"
    );
    assert!(
        json.contains("\"resources\":[\"file:///notes/1.md\"]"),
        "资源列表: {json}"
    );
    let back: McpSubscriptionsConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, sub, "round-trip 后必须与原始值一致");
}

/// McpServerConfig.subscriptions 缺省兼容：未提供时解析为 None（#[serde(default)]）。
#[test]
fn test_mcp_server_config_subscriptions_defaults_to_none() {
    let cfg: McpServerConfig = serde_json::from_str(r#"{"command":"npx"}"#).unwrap();
    assert!(
        cfg.subscriptions.is_none(),
        "缺省 subscriptions 应解析为 None"
    );
}

/// McpServerConfig.subscriptions 显式提供时按 camelCase 解析。
#[test]
fn test_mcp_server_config_subscriptions_parses_camel_case() {
    let cfg: McpServerConfig = serde_json::from_str(
        r#"{"command":"npx","subscriptions":{"toolsListChanged":true,"resources":["file:///a.md"]}}"#,
    )
    .unwrap();
    let sub = cfg.subscriptions.expect("显式 subscriptions 应解析成功");
    assert!(
        sub.tools_list_changed,
        "toolsListChanged 应按 camelCase 解析"
    );
    assert_eq!(sub.resources, vec!["file:///a.md".to_string()]);
}

/// subscriptions 未配置时不序列化（skip_serializing_if = "Option::is_none"）。
#[test]
fn test_mcp_server_config_subscriptions_skipped_when_none() {
    let cfg: McpServerConfig = serde_json::from_str(r#"{"command":"npx"}"#).unwrap();
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(
        !json.contains("subscriptions"),
        "未配置时不得序列化: {json}"
    );
}
