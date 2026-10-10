use super::TuiConfig;
use serde_json::{json, Map};

#[test]
fn test_default_config_has_empty_extra_and_disabled_options() {
    let config = TuiConfig::default();
    assert!(!config.diff_enabled);
    assert!(config.streaming_mode.is_none());
    assert!(config.scroll_fps.is_none());
    assert!(config.theme.is_none());
    assert!(!config.daily_color);
    assert!(config.daily_color_date.is_none());
    assert!(config.extra.is_empty());
}

#[test]
fn test_from_extra_reads_known_fields_and_ignores_unknown_fields() {
    let extra = serde_json::from_value::<Map<String, serde_json::Value>>(json!({
        "diff_enabled": true,
        "streaming_mode": "block",
        "scroll_fps": 60,
        "theme": "peri-dark",
        "daily_color": true,
        "daily_color_date": "2026-10-01",
        "custom": {"preserved_elsewhere": true}
    }))
    .unwrap();
    let config = TuiConfig::from_extra(&extra);
    assert!(config.diff_enabled);
    assert_eq!(config.streaming_mode.as_deref(), Some("block"));
    assert_eq!(config.scroll_fps, Some(60));
    assert_eq!(config.theme.as_deref(), Some("peri-dark"));
    assert!(config.daily_color);
    assert_eq!(config.daily_color_date.as_deref(), Some("2026-10-01"));
    assert!(config.extra.is_empty());
}

#[test]
fn test_from_extra_uses_defaults_for_wrong_field_types() {
    let extra = serde_json::from_value::<Map<String, serde_json::Value>>(json!({
        "diff_enabled": "yes",
        "streaming_mode": false,
        "scroll_fps": -1,
        "theme": null,
        "daily_color": 1,
        "daily_color_date": 20261001
    }))
    .unwrap();
    let config = TuiConfig::from_extra(&extra);
    assert!(!config.diff_enabled);
    assert!(config.streaming_mode.is_none());
    assert!(config.scroll_fps.is_none());
    assert!(config.theme.is_none());
    assert!(!config.daily_color);
    assert!(config.daily_color_date.is_none());
}

#[test]
fn test_sync_to_extra_updates_known_fields_and_removes_unset_optional_values() {
    let mut extra = serde_json::from_value::<Map<String, serde_json::Value>>(json!({
        "streaming_mode": "old",
        "scroll_fps": 30,
        "theme": "old-theme",
        "daily_color_date": "old-date",
        "custom": true
    }))
    .unwrap();
    TuiConfig {
        diff_enabled: true,
        daily_color: true,
        ..Default::default()
    }
    .sync_to_extra(&mut extra);
    assert_eq!(extra.get("diff_enabled"), Some(&json!(true)));
    assert_eq!(extra.get("daily_color"), Some(&json!(true)));
    assert!(!extra.contains_key("streaming_mode"));
    assert!(!extra.contains_key("scroll_fps"));
    assert!(!extra.contains_key("theme"));
    assert!(!extra.contains_key("daily_color_date"));
    assert_eq!(extra.get("custom"), Some(&json!(true)));
}

#[test]
fn test_serde_roundtrip_preserves_flattened_fields() {
    let config = TuiConfig {
        diff_enabled: true,
        extra: serde_json::from_value(json!({"custom": "value"})).unwrap(),
        ..Default::default()
    };
    let serialized = serde_json::to_value(config).unwrap();
    let restored: TuiConfig = serde_json::from_value(serialized).unwrap();
    assert!(restored.diff_enabled);
    assert_eq!(restored.extra.get("custom"), Some(&json!("value")));
}
