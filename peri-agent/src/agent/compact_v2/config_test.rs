use std::env;
use std::sync::Mutex;

use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_default_values() {
    let config = CompactConfig::default();
    assert!(config.auto_compact_enabled);
    assert!((config.auto_compact_threshold - 0.95).abs() < 0.001);
    assert!((config.micro_compact_threshold - 0.75).abs() < 0.001);
    assert_eq!(config.micro_compact_stale_steps, 2);
    assert_eq!(config.micro_min_gain_divisor, 10);
    assert!(config.has_valid_micro_min_gain_divisor());
    assert_eq!(config.micro_field_threshold_chars, 500);
    assert_eq!(config.micro_field_keep_head_chars, 350);
    assert_eq!(config.micro_field_keep_tail_chars, 100);
    assert!(config.has_valid_micro_field_limits());
    assert!(
        config.micro_excluded_tools.is_empty(),
        "工具目录提供语义保护，不按名字永久豁免"
    );
    assert_eq!(config.summary_max_tokens, 16000);
    assert_eq!(config.summary_max_requests, 3);
    assert_eq!(config.max_consecutive_failures, 3);
    assert_eq!(
        config.max_resident_history_bytes,
        128 * 1024 * 1024,
        "驻留预算默认 128 MiB"
    );
}

#[test]
fn test_micro_min_gain_divisor_default_override_and_zero_rejection() {
    assert_eq!(CompactConfig::default().micro_min_gain_divisor, 10);

    let overridden: CompactConfig =
        serde_json::from_str(r#"{"micro_min_gain_divisor":20}"#).unwrap();
    assert_eq!(overridden.micro_min_gain_divisor, 20);

    assert!(
        serde_json::from_str::<CompactConfig>(r#"{"micro_min_gain_divisor":0}"#).is_err(),
        "零值不是「不限」，外部配置必须被拒绝"
    );

    let zero = CompactConfig {
        micro_min_gain_divisor: 0,
        ..Default::default()
    };
    assert!(!zero.has_valid_micro_min_gain_divisor());
}

#[test]
fn test_max_resident_history_bytes_default_deserialize_and_override() {
    assert_eq!(
        CompactConfig::default().max_resident_history_bytes,
        134_217_728
    );

    let config: CompactConfig = serde_json::from_str(r#"{"auto_compact_threshold":0.9}"#).unwrap();
    assert_eq!(
        config.max_resident_history_bytes, 134_217_728,
        "缺省 JSON 字段使用 128 MiB 默认值"
    );

    let config: CompactConfig =
        serde_json::from_str(r#"{"max_resident_history_bytes":4096}"#).unwrap();
    assert_eq!(config.max_resident_history_bytes, 4096);

    let config: CompactConfig =
        serde_json::from_str(r#"{"max_resident_history_bytes":0}"#).unwrap();
    assert_eq!(config.max_resident_history_bytes, 0, "0 表示关闭驻留门控");

    let serialized = serde_json::to_string(&CompactConfig {
        max_resident_history_bytes: 8192,
        ..Default::default()
    })
    .unwrap();
    let back: CompactConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(back.max_resident_history_bytes, 8192);
}

#[test]
fn test_serde_roundtrip() {
    let config = CompactConfig {
        auto_compact_threshold: 0.90,
        micro_compact_stale_steps: 10,
        micro_field_threshold_chars: 900,
        micro_field_keep_head_chars: 600,
        micro_field_keep_tail_chars: 200,
        summary_max_tokens: 8000,
        summary_max_requests: 5,
        ..Default::default()
    };
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: CompactConfig = serde_json::from_str(&json).unwrap();
    assert!((deserialized.auto_compact_threshold - 0.90).abs() < 0.001);
    assert_eq!(deserialized.micro_compact_stale_steps, 10);
    assert_eq!(deserialized.micro_field_threshold_chars, 900);
    assert_eq!(deserialized.micro_field_keep_head_chars, 600);
    assert_eq!(deserialized.micro_field_keep_tail_chars, 200);
    assert!(deserialized.has_valid_micro_field_limits());
    assert_eq!(deserialized.summary_max_tokens, 8000);
    assert_eq!(deserialized.summary_max_requests, 5);
    assert!((deserialized.micro_compact_threshold - 0.75).abs() < 0.001);
}

#[test]
fn test_micro_field_limits_reject_zero_threshold_and_non_shrinking_values() {
    let zero_threshold = CompactConfig {
        micro_field_threshold_chars: 0,
        ..Default::default()
    };
    let non_shrinking = CompactConfig {
        micro_field_threshold_chars: 500,
        micro_field_keep_head_chars: 350,
        micro_field_keep_tail_chars: 150,
        ..Default::default()
    };
    assert!(!zero_threshold.has_valid_micro_field_limits());
    assert!(!non_shrinking.has_valid_micro_field_limits());
}

#[test]
fn test_serde_partial_deserialize() {
    let json = r#"{"auto_compact_threshold": 0.90}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.90).abs() < 0.001);
    assert!(config.auto_compact_enabled);
    assert!((config.micro_compact_threshold - 0.75).abs() < 0.001);
    assert_eq!(config.summary_max_requests, 3);
}

#[test]
fn test_serde_empty_object() {
    let json = "{}";
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!(config.auto_compact_enabled);
    assert!((config.auto_compact_threshold - 0.95).abs() < 0.001);
}

#[test]
fn test_apply_env_overrides_on_custom_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    env::remove_var("DISABLE_COMPACT");
    env::remove_var("DISABLE_AUTO_COMPACT");
    env::set_var("COMPACT_THRESHOLD", "0.80");
    let mut config = CompactConfig {
        auto_compact_threshold: 0.90,
        ..Default::default()
    };
    config.apply_env_overrides();
    env::remove_var("COMPACT_THRESHOLD");
    assert!((config.auto_compact_threshold - 0.80).abs() < 0.001);
}

#[test]
fn test_excluded_tools_are_explicit_additional_protection() {
    let config: CompactConfig =
        serde_json::from_str(r#"{"micro_excluded_tools":["CustomTool"]}"#).unwrap();
    assert_eq!(config.micro_excluded_tools, vec!["CustomTool"]);
    assert!(CompactConfig::default().micro_excluded_tools.is_empty());
}

#[test]
fn test_auto_compact_threshold_clamps_out_of_range() {
    // 超出 1.0 → clamp 到 1.0
    let json = r#"{"auto_compact_threshold": 1.5}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 1.0).abs() < 0.001);

    // 负数 → clamp 到 0.0
    let json = r#"{"auto_compact_threshold": -0.5}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.0).abs() < 0.001);
}

#[test]
fn test_auto_compact_threshold_normal_values_unchanged() {
    // 合法范围内的值保持不变
    let json = r#"{"auto_compact_threshold": 0.80}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.80).abs() < 0.001);

    let json = r#"{"auto_compact_threshold": 0.95}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.95).abs() < 0.001);

    let json = r#"{"auto_compact_threshold": 0.0}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.0).abs() < 0.001);

    let json = r#"{"auto_compact_threshold": 1.0}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 1.0).abs() < 0.001);
}

#[test]
fn test_auto_compact_threshold_default_is_valid() {
    let config = CompactConfig::default();
    assert!((config.auto_compact_threshold - 0.95).abs() < 0.001);
    assert!(config.auto_compact_threshold >= 0.0 && config.auto_compact_threshold <= 1.0);
}
