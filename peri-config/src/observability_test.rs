//! 旧观测键与整体环境键表的解析回归（契约 §8：旧键无回归）。
//!
//! 新增的 spool/compression 新键用例已按 §8 映射迁至 `observability_spool_test.rs`。

use super::{resolve, LangfuseConfig, ENVIRONMENT_KEYS};
use serde_json::json;
use std::collections::BTreeMap;

fn resolve_ok(
    settings: &serde_json::Value,
    environment: &BTreeMap<String, String>,
) -> LangfuseConfig {
    resolve(settings, environment).expect("测试配置必须解析成功")
}

/// §6 的 11 个新键与默认值。
const NEW_DEFAULTS: &[(&str, &str)] = &[
    ("LANGFUSE_SPOOL_ENABLED", "true"),
    ("LANGFUSE_COMPRESSION", "gzip"),
    ("LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES", "262144"),
    ("LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES", "262144"),
    ("LANGFUSE_MEMORY_BUDGET_BYTES", "33554432"),
    ("LANGFUSE_PREPARE_MAX_IN_FLIGHT", "1"),
    ("LANGFUSE_STREAM_CHUNK_BYTES", "65536"),
    ("LANGFUSE_SPOOL_QUOTA_BYTES", "1073741824"),
    ("LANGFUSE_SPOOL_TTL_SECS", "86400"),
    ("LANGFUSE_MAX_WIRE_BYTES", "4194304"),
    ("LANGFUSE_SHUTDOWN_GRACE_SECS", "60"),
];

#[test]
fn test_defaults_are_preserved() {
    let config = resolve_ok(&json!({}), &BTreeMap::new());
    assert!(config.public_key.is_none());
    assert!(config.secret_key.is_none());
    assert_eq!(config.host, "https://cloud.langfuse.com");
    assert_eq!(config.trace_sampling, 1.0);
    assert!(config.error_span_always);
    assert_eq!(config.batch_max_events, 50);
    assert_eq!(config.batch_queue_capacity, 1024);
    assert_eq!(config.batch_max_in_flight, 2);
    assert_eq!(config.batch_max_event_bytes, 4 * 1024 * 1024);
    assert_eq!(config.batch_max_bytes, 4 * 1024 * 1024);
    assert_eq!(config.batch_max_queue_bytes, 16 * 1024 * 1024);
    assert_eq!(config.batch_flush_interval_secs, 10);
    assert!(config.user_id.is_none());
}

#[test]
fn test_settings_values_are_parsed_and_sampling_is_clamped() {
    let config = resolve_ok(
        &json!({"langfuse": {
            "trace_sampling": 2.5,
            "error_span_always": false,
            "batch_max_events": 100,
            "batch_flush_interval_secs": 30,
            "batch_queue_capacity": 200,
            "batch_max_in_flight": 4,
            "batch_max_event_bytes": 1000,
            "batch_max_bytes": 4000,
            "batch_max_queue_bytes": 16000
        }}),
        &BTreeMap::new(),
    );
    assert_eq!(config.trace_sampling, 1.0);
    assert!(!config.error_span_always);
    assert_eq!(config.batch_max_events, 100);
    assert_eq!(config.batch_flush_interval_secs, 30);
    assert_eq!(config.batch_queue_capacity, 200);
    assert_eq!(config.batch_max_in_flight, 4);
    assert_eq!(config.batch_max_event_bytes, 1000);
    assert_eq!(config.batch_max_bytes, 4000);
    assert_eq!(config.batch_max_queue_bytes, 16000);
}

#[test]
fn test_environment_overrides_settings_and_invalid_values_are_ignored() {
    let environment = BTreeMap::from([
        ("LANGFUSE_PUBLIC_KEY".to_string(), "public".to_string()),
        ("LANGFUSE_SECRET_KEY".to_string(), "secret".to_string()),
        (
            "LANGFUSE_BASE_URL".to_string(),
            "https://example.test".to_string(),
        ),
        ("LANGFUSE_TRACE_SAMPLING".to_string(), "0.7".to_string()),
        ("LANGFUSE_ERROR_SPAN_ALWAYS".to_string(), "0".to_string()),
        ("LANGFUSE_BATCH_MAX_EVENTS".to_string(), "bad".to_string()),
        (
            "LANGFUSE_BATCH_QUEUE_CAPACITY".to_string(),
            "300".to_string(),
        ),
        (
            "LANGFUSE_BATCH_FLUSH_INTERVAL".to_string(),
            "bad".to_string(),
        ),
        ("LANGFUSE_USER_ID".to_string(), "operator".to_string()),
    ]);
    let config = resolve_ok(
        &json!({"langfuse": {
            "trace_sampling": 0.3,
            "error_span_always": true,
            "batch_max_events": 100,
            "batch_flush_interval_secs": 30,
            "batch_queue_capacity": 200
        }}),
        &environment,
    );
    assert_eq!(config.public_key.as_deref(), Some("public"));
    assert_eq!(config.secret_key.as_deref(), Some("secret"));
    assert_eq!(config.host, "https://example.test");
    assert_eq!(config.trace_sampling, 0.7);
    assert!(!config.error_span_always);
    assert_eq!(config.batch_max_events, 100);
    assert_eq!(config.batch_flush_interval_secs, 30);
    assert_eq!(config.batch_queue_capacity, 300);
    assert_eq!(config.user_id.as_deref(), Some("operator"));
}

#[test]
fn test_environment_boolean_and_sampling_edge_values_are_preserved() {
    let environment = BTreeMap::from([
        ("LANGFUSE_TRACE_SAMPLING".to_string(), "-1.5".to_string()),
        (
            "LANGFUSE_ERROR_SPAN_ALWAYS".to_string(),
            "False".to_string(),
        ),
    ]);
    let config = resolve_ok(&json!({}), &environment);
    assert_eq!(config.trace_sampling, 0.0);
    assert!(!config.error_span_always);
}

#[test]
fn test_environment_key_list_includes_all_supported_overrides() {
    assert_eq!(ENVIRONMENT_KEYS.len(), 24);
    assert!(ENVIRONMENT_KEYS.contains(&"LANGFUSE_SECRET_KEY"));
    assert!(ENVIRONMENT_KEYS.contains(&"LANGFUSE_BATCH_MAX_QUEUE_BYTES"));
    for (key, _) in NEW_DEFAULTS {
        assert!(ENVIRONMENT_KEYS.contains(key), "缺少新键 {key}");
    }
    let mut unique = ENVIRONMENT_KEYS.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ENVIRONMENT_KEYS.len(), "环境键不得重复");
}

#[test]
fn debug_output_preserves_endpoint_credentials_and_user_identity() {
    let config = resolve_ok(
        &json!({}),
        &BTreeMap::from([
            (
                "LANGFUSE_BASE_URL".into(),
                "https://hidden-token@example.test".into(),
            ),
            ("LANGFUSE_USER_ID".into(), "hidden-user".into()),
        ]),
    );
    let rendered = format!("{config:?}");
    assert!(rendered.contains("hidden-token"));
    assert!(rendered.contains("hidden-user"));
}

#[test]
fn test_debug_preserves_credentials() {
    let config = LangfuseConfig {
        public_key: Some("public-sensitive".to_string()),
        secret_key: Some("secret-sensitive".to_string()),
        ..Default::default()
    };
    let debug = format!("{config:?}");
    assert!(debug.contains("public-sensitive"));
    assert!(debug.contains("secret-sensitive"));
    assert!(!debug.contains("[REDACTED]"));
}
