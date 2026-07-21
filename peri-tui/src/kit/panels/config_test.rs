//! Tests for panels_config

#[cfg(test)]
use super::*;

#[cfg(test)]
use crate::config::{PeriConfig, TuiConfig};

#[test]
fn test_apply_toggle_row_show_diff_flips() {
    let mut cfg = TuiConfig::default();
    assert!(!cfg.diff_enabled);
    assert!(!cfg.diff_enabled);
    cfg.diff_enabled = !cfg.diff_enabled;
    assert!(cfg.diff_enabled);
    cfg.diff_enabled = !cfg.diff_enabled;
    assert!(!cfg.diff_enabled);
}

#[test]
fn test_apply_toggle_row_cache_warn_flips() {
    let mut cfg = PeriConfig::default();
    let initial = cfg.config.show_cache_warning;
    let new = apply_toggle_row(&mut cfg, ROW_CACHE_WARN);
    assert_eq!(new, Some(!initial));
    assert_eq!(cfg.config.show_cache_warning, !initial);
}

#[test]
fn test_apply_toggle_row_1m_context_handles_none_initial() {
    // 默认 context_1m = None（unwrap_or(false) → false → toggle 为 true）
    let mut cfg = PeriConfig::default();
    assert_eq!(cfg.config.context_1m, None);
    let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
    assert_eq!(new, Some(true));
    assert_eq!(cfg.config.context_1m, Some(true));
    let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
    assert_eq!(new, Some(false));
    assert_eq!(cfg.config.context_1m, Some(false));
}

#[test]
fn test_apply_toggle_row_invalid_returns_none() {
    let mut cfg = PeriConfig::default();
    // ROW_STREAMING 是 Cycle 不是 Toggle——应返回 None
    assert_eq!(apply_toggle_row(&mut cfg, ROW_STREAMING), None);
    // 越界 row
    assert_eq!(apply_toggle_row(&mut cfg, 99), None);
}

#[test]
fn test_apply_cycle_row_streaming_forward_wraps() {
    let mut cfg = TuiConfig {
        streaming_mode: Some("none".into()),
        ..Default::default()
    };
    let next = apply_cycle_row_tui(&mut cfg, ROW_STREAMING, true);
    assert_eq!(next, Some(0)); // wrap to streaming
    assert_eq!(cfg.streaming_mode.as_deref(), Some("streaming"));
}

#[test]
fn test_apply_cycle_row_alias_backward() {
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into(); // idx=0
    let prev = apply_cycle_row(&mut cfg, ROW_ACTIVE_ALIAS, false);
    assert_eq!(prev, Some(2)); // wrap to haiku
    assert_eq!(cfg.config.active_alias, "haiku");
}

#[test]
fn test_apply_cycle_row_language_forward_from_unknown_resets() {
    // 当前值为非选项时，unwrap_or(0) → 视为 idx=0，forward 后到 idx=1
    let mut cfg = PeriConfig::default();
    cfg.config.language = Some("fr".into()); // 非合法选项
    let next = apply_cycle_row(&mut cfg, ROW_LANGUAGE, true);
    assert_eq!(next, Some(1));
    assert_eq!(cfg.config.language.as_deref(), Some("zh-CN"));
}

#[test]
fn test_apply_cycle_row_invalid_returns_none() {
    let mut cfg = PeriConfig::default();
    // ROW_SHOW_DIFF 是 Toggle 不是 Cycle——应返回 None
    assert_eq!(apply_cycle_row(&mut cfg, ROW_SHOW_DIFF, true), None);
    assert_eq!(apply_cycle_row(&mut cfg, 99, true), None);
}

#[test]
fn test_parse_permission_mode_roundtrip() {
    for opt in PERMISSION_OPTS {
        let mode = parse_permission_mode(opt);
        assert!(mode.is_some(), "{} 应解析成功", opt);
        let label = permission_mode_label(mode.unwrap());
        assert_eq!(label, *opt, "label 与原始字符串应一致");
    }
}

#[test]
fn test_parse_permission_mode_invalid() {
    assert!(parse_permission_mode("invalid").is_none());
    assert!(parse_permission_mode("").is_none());
}
