//! ThreadMeta 强类型字段与枚举测试。
//!
//! 覆盖：
//! - 默认值（new/default_for_db/反序列化缺字段）
//! - 强类型枚举 CancelPolicy / AgentStatus 的 FromStr/Display/as_str
//! - 非法字符串不静默 fallback 的硬约束
use super::*;

#[test]
fn test_thread_meta_default_values() {
    // new() 创建的根线程应具有正确的默认值
    let meta = ThreadMeta::new("/tmp/test");
    assert_eq!(meta.parent_thread_id, None);
    assert_eq!(meta.snapshot_at_message_id, None);
    assert!(!meta.hidden);
    assert_eq!(meta.cancel_policy, CancelPolicy::Cascade);
    assert_eq!(meta.config, None);
    assert_eq!(meta.cached_context, None);
    assert_eq!(meta.agent_status, AgentStatus::Active);
    assert!(meta.is_root());
}

#[test]
fn test_thread_meta_default_for_db_uses_typed_defaults() {
    // default_for_db 应填充强类型默认值（DB 读路径占位）
    let meta = ThreadMeta::default_for_db();
    assert_eq!(meta.cancel_policy, CancelPolicy::Cascade);
    assert_eq!(meta.agent_status, AgentStatus::Active);
}

#[test]
fn test_thread_meta_is_root() {
    // parent_thread_id 为 None 时是根 agent
    let mut meta = ThreadMeta::new("/tmp/test");
    assert!(meta.is_root());

    // 设置 parent_thread_id 后不是根 agent
    meta.parent_thread_id = Some("parent-uuid".to_string());
    assert!(!meta.is_root());
}

#[test]
fn test_thread_meta_deserialize_defaults() {
    // 反序列化旧格式 JSON（缺 cancel_policy / agent_status）时应使用 serde default
    let json = r#"{"id":"test-id","title":null,"cwd":"/tmp","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","message_count":0,"content_size":0}"#;
    let meta: ThreadMeta = serde_json::from_str(json).unwrap();
    assert_eq!(meta.parent_thread_id, None);
    assert_eq!(meta.snapshot_at_message_id, None);
    assert!(!meta.hidden);
    assert_eq!(meta.cancel_policy, CancelPolicy::Cascade);
    assert_eq!(meta.config, None);
    assert_eq!(meta.cached_context, None);
    assert_eq!(meta.agent_status, AgentStatus::Active);
}

#[test]
fn test_thread_meta_serialize_roundtrip_typed_fields() {
    // 强类型字段经 JSON 往返后应保持等价
    let meta = ThreadMeta::new("/tmp");
    let json = serde_json::to_string(&meta).unwrap();
    let back: ThreadMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(back.cancel_policy, meta.cancel_policy);
    assert_eq!(back.agent_status, meta.agent_status);
    // 序列化结果应是 lowercase 字符串
    assert!(json.contains("\"cascade\""));
    assert!(json.contains("\"active\""));
}

// ── CancelPolicy 强类型 ─────────────────────────────────────────────────────

#[test]
fn test_cancel_policy_as_str_round_trip() {
    assert_eq!(CancelPolicy::Cascade.as_str(), "cascade");
    assert_eq!(CancelPolicy::Independent.as_str(), "independent");
    assert_eq!(CancelPolicy::Cascade.to_string(), "cascade");
}

#[test]
fn test_cancel_policy_from_str_legal() {
    assert_eq!(
        "cascade".parse::<CancelPolicy>().unwrap(),
        CancelPolicy::Cascade
    );
    assert_eq!(
        "independent".parse::<CancelPolicy>().unwrap(),
        CancelPolicy::Independent
    );
}

#[test]
fn test_cancel_policy_from_str_illegal_returns_err_no_fallback() {
    // 关键约束：非法值必须报错，禁止静默 fallback 到 Cascade
    let err = "unknown".parse::<CancelPolicy>().unwrap_err();
    assert!(matches!(
        err,
        ThreadMetaParseError::InvalidCancelPolicy(ref s) if s == "unknown"
    ));
    // 大小写敏感：Cascade 不应匹配
    assert!("Cascade".parse::<CancelPolicy>().is_err());
    assert!("".parse::<CancelPolicy>().is_err());
}

#[test]
fn test_cancel_policy_serde_lowercase() {
    let json = serde_json::to_string(&CancelPolicy::Independent).unwrap();
    assert_eq!(json, "\"independent\"");
    let back: CancelPolicy = serde_json::from_str("\"cascade\"").unwrap();
    assert_eq!(back, CancelPolicy::Cascade);
}

// ── AgentStatus 强类型 ──────────────────────────────────────────────────────

#[test]
fn test_agent_status_as_str_round_trip() {
    assert_eq!(AgentStatus::Active.as_str(), "active");
    assert_eq!(AgentStatus::Done.as_str(), "done");
    assert_eq!(AgentStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(AgentStatus::Error.as_str(), "error");
    assert_eq!(AgentStatus::Done.to_string(), "done");
}

#[test]
fn test_agent_status_is_active() {
    assert!(AgentStatus::Active.is_active());
    assert!(!AgentStatus::Done.is_active());
    assert!(!AgentStatus::Cancelled.is_active());
    assert!(!AgentStatus::Error.is_active());
}

#[test]
fn test_agent_status_from_str_legal() {
    assert_eq!(
        "active".parse::<AgentStatus>().unwrap(),
        AgentStatus::Active
    );
    assert_eq!("done".parse::<AgentStatus>().unwrap(), AgentStatus::Done);
    assert_eq!(
        "cancelled".parse::<AgentStatus>().unwrap(),
        AgentStatus::Cancelled
    );
    assert_eq!("error".parse::<AgentStatus>().unwrap(), AgentStatus::Error);
}

#[test]
fn test_agent_status_from_str_illegal_returns_err_no_fallback() {
    // 关键约束：非法值必须报错，禁止静默 fallback 到 Active
    let err = "running".parse::<AgentStatus>().unwrap_err();
    assert!(matches!(
        err,
        ThreadMetaParseError::InvalidAgentStatus(ref s) if s == "running"
    ));
    assert!("Active".parse::<AgentStatus>().is_err());
    assert!("".parse::<AgentStatus>().is_err());
}

#[test]
fn test_agent_status_serde_lowercase() {
    let json = serde_json::to_string(&AgentStatus::Cancelled).unwrap();
    assert_eq!(json, "\"cancelled\"");
    let back: AgentStatus = serde_json::from_str("\"error\"").unwrap();
    assert_eq!(back, AgentStatus::Error);
}
