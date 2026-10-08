//! `config.betas` 覆盖表测试：解析、合并、差异提取（设计 §配置面接入）。

use super::*;

/// 覆盖表按 bool 稀疏 map 解析，缺省为空（不落盘）。
#[test]
fn betas_parse_and_default_empty() {
    let cfg = AppConfig::default();
    assert!(cfg.betas.is_default());
    let value = serde_json::to_value(&cfg).unwrap();
    assert!(
        !value.as_object().unwrap().contains_key("betas"),
        "空覆盖表不序列化"
    );

    let json = r#"{"betas":{"full-async-tools":true}}"#;
    let parsed: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.betas.get("full-async-tools"), Some(true));
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(back["betas"]["full-async-tools"], serde_json::json!(true));
}

/// 非 bool 值使该来源解析失败（不静默降级为 false/true）。
#[test]
fn betas_reject_non_bool_values() {
    let error = serde_json::from_str::<AppConfig>(r#"{"betas":{"full-async-tools":"yes"}}"#)
        .expect_err("字符串值必须解析失败");
    assert!(error.to_string().contains("invalid type"), "{error}");
}

/// 未知键在解析后剔除并保留已知键（不进入快照/投影/面板）。
#[test]
fn betas_unknown_key_is_dropped() {
    let json = r#"{"betas":{"full-async-tools":true,"not-a-flag":true}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_overrides();
    assert_eq!(cfg.betas.get("full-async-tools"), Some(true));
    assert_eq!(
        cfg.betas.get("not-a-flag"),
        None,
        "未知 flag id 从内存配置剔除"
    );
}

/// 未知键不影响其他配置领域。
#[test]
fn betas_unknown_key_does_not_disturb_other_domains() {
    let json = r#"{"language":"zh-CN","show_cache_warning":true,"betas":{"typo-flag":true}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_overrides();
    assert!(cfg.betas.is_default());
    assert_eq!(cfg.language.as_deref(), Some("zh-CN"));
    assert_eq!(cfg.show_cache_warning, Some(true));
}

/// global → workspace 逐 key 合并（workspace 胜出，新键追加）。
#[test]
fn betas_merge_per_key() {
    let mut global = AppConfig::default();
    global.betas.set("full-async-tools", true);
    let mut workspace = AppConfig::default();
    workspace.betas.set("other-flag", true);
    global.merge_overrides(workspace);
    assert_eq!(global.betas.get("full-async-tools"), Some(true));
    assert_eq!(global.betas.get("other-flag"), Some(true));
}

/// workspace 显式 false 关闭 global 的 true（不做整体替换）。
#[test]
fn betas_workspace_false_closes_global_true() {
    let mut global = AppConfig::default();
    global.betas.set("full-async-tools", true);
    let mut workspace = AppConfig::default();
    workspace.betas.set("full-async-tools", false);
    global.merge_overrides(workspace);
    assert_eq!(global.betas.get("full-async-tools"), Some(false));
}

/// 差异提取：与 global 同值的键剔除，覆盖键保留（workspace 保存只写差异）。
#[test]
fn betas_extract_overrides_keeps_only_diff() {
    let mut global = AppConfig::default();
    global.betas.set("full-async-tools", true);
    let mut merged = global.clone();
    merged.betas.set("full-async-tools", false);
    merged.betas.set("workspace-only", true);
    let extracted = merged.extract_overrides(&global);
    assert_eq!(extracted.betas.get("full-async-tools"), Some(false));
    assert_eq!(extracted.betas.get("workspace-only"), Some(true));
    assert_eq!(extracted.betas.overrides.len(), 2);

    let mut merged_same = global.clone();
    merged_same.betas.set("full-async-tools", true);
    let extracted = merged_same.extract_overrides(&global);
    assert!(
        extracted.betas.is_default(),
        "与 global 同值的键不写回工作区"
    );
}
