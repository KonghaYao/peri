use std::sync::Mutex;

use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_default_values() {
    let config = CompactConfig::default();
    assert!(config.auto_compact_enabled);
    assert!((config.auto_compact_threshold - 0.95).abs() < 0.001);
    assert!((config.micro_compact_threshold - 0.75).abs() < 0.001);
    assert_eq!(config.micro_compact_stale_steps, 3);
    assert_eq!(
        config.micro_excluded_tools.len(),
        3,
        "黑名单默认 3 个工具（AskUserQuestion/goal/TodoWrite）"
    );
    assert_eq!(config.summary_max_tokens, 16000);
    assert_eq!(config.re_inject_max_files, 5);
    assert_eq!(config.re_inject_max_tokens_per_file, 5000);
    assert_eq!(config.re_inject_file_budget, 25000);
    assert_eq!(config.re_inject_skills_budget, 25000);
    assert_eq!(config.max_consecutive_failures, 3);
    assert_eq!(config.ptl_max_retries, 3);
}

#[test]
fn test_serde_roundtrip() {
    let config = CompactConfig {
        auto_compact_threshold: 0.90,
        micro_compact_stale_steps: 10,
        summary_max_tokens: 8000,
        ..Default::default()
    };
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: CompactConfig = serde_json::from_str(&json).unwrap();
    assert!((deserialized.auto_compact_threshold - 0.90).abs() < 0.001);
    assert_eq!(deserialized.micro_compact_stale_steps, 10);
    assert_eq!(deserialized.summary_max_tokens, 8000);
    assert!((deserialized.micro_compact_threshold - 0.75).abs() < 0.001);
}

#[test]
fn test_serde_partial_deserialize() {
    let json = r#"{"auto_compact_threshold": 0.90}"#;
    let config: CompactConfig = serde_json::from_str(json).unwrap();
    assert!((config.auto_compact_threshold - 0.90).abs() < 0.001);
    assert!(config.auto_compact_enabled);
    assert!((config.micro_compact_threshold - 0.75).abs() < 0.001);
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
fn test_excluded_tools_default_non_empty() {
    let config = CompactConfig::default();
    assert!(
        !config.micro_excluded_tools.is_empty(),
        "黑名单默认应包含 AskUserQuestion/goal/TodoWrite"
    );
    assert!(
        config
            .micro_excluded_tools
            .contains(&"AskUserQuestion".to_string()),
        "默认应包含 AskUserQuestion"
    );
    assert!(
        config.micro_excluded_tools.contains(&"goal".to_string()),
        "默认应包含 goal"
    );
    assert!(
        config
            .micro_excluded_tools
            .contains(&"TodoWrite".to_string()),
        "默认应包含 TodoWrite"
    );
}
