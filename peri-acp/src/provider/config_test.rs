use std::collections::HashMap;

use super::*;

fn make_global() -> AppConfig {
    AppConfig {
        active_alias: "sonnet".to_string(),
        providers: vec![ProviderConfig {
            id: "openai-1".to_string(),
            provider_type: "openai".to_string(),
            api_key: "sk-global".to_string(),
            ..Default::default()
        }],
        profiles: Profiles {
            sonnet: ProfileConfig {
                effort: "medium".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        language: Some("zh".to_string()),
        ..Default::default()
    }
}

#[test]
fn test_merge_workspace_default_preserves_most_fields() {
    let mut global = make_global();
    let workspace = AppConfig::default();
    global.merge_overrides(workspace);
    assert_eq!(global.active_alias, "sonnet");
    assert_eq!(global.providers.len(), 1);
    assert_eq!(global.profiles.sonnet.effort, "medium");
}

#[test]
fn test_merge_workspace_complete_overrides_all() {
    let mut global = make_global();
    let workspace = AppConfig {
        active_alias: "opus".to_string(),
        providers: vec![ProviderConfig {
            id: "anthro-1".to_string(),
            provider_type: "anthropic".to_string(),
            api_key: "sk-ws".to_string(),
            ..Default::default()
        }],
        language: Some("en".to_string()),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(global.active_alias, "opus");
    assert_eq!(global.providers.len(), 1);
    assert_eq!(global.providers[0].provider_type, "anthropic");
    assert_eq!(global.language, Some("en".to_string()));
    assert_eq!(global.profiles.sonnet.effort, "medium");
}

#[test]
fn test_merge_providers_empty_array_does_not_override() {
    let mut global = make_global();
    let workspace = AppConfig {
        providers: vec![],
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(global.providers.len(), 1);
    assert_eq!(global.providers[0].api_key, "sk-global");
}

#[test]
fn test_merge_single_field_override() {
    let mut global = make_global();
    let workspace = AppConfig {
        active_alias: "haiku".to_string(),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(global.active_alias, "haiku");
    assert_eq!(global.providers.len(), 1);
    assert_eq!(global.providers[0].api_key, "sk-global");
}

#[test]
fn test_merge_env_override() {
    let mut global = AppConfig {
        env: Some(HashMap::from([("FOO".to_string(), "bar".to_string())])),
        ..make_global()
    };
    let workspace = AppConfig {
        env: Some(HashMap::from([("BAZ".to_string(), "qux".to_string())])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    let env = global.env.unwrap();
    assert!(!env.contains_key("FOO"));
    assert_eq!(env.get("BAZ"), Some(&"qux".to_string()));
}

#[test]
fn test_merge_json_workspace_overrides_single_field() {
    let mut global = make_global(); // active_alias: "sonnet"
    let json = r#"{"active_alias":"haiku"}"#;
    let workspace: AppConfig = serde_json::from_str(json).unwrap();
    global.merge_overrides(workspace);
    assert_eq!(global.active_alias, "haiku");
    // show_cache_warning: workspace 未显式设置（None）→ 保留全局值，不被默认覆盖
    assert_eq!(global.show_cache_warning, None);
    // Other fields preserved from global
    assert_eq!(global.providers.len(), 1);
    assert_eq!(global.profiles.sonnet.effort, "medium");
}

#[test]
fn test_merge_workspace_not_set_preserves_global_cache_warning() {
    let mut global = make_global();
    global.show_cache_warning = Some(true);
    // workspace 只设置 active_alias，未写 show_cache_warning
    let json = r#"{"active_alias":"haiku"}"#;
    let workspace: AppConfig = serde_json::from_str(json).unwrap();
    global.merge_overrides(workspace);
    assert_eq!(
        global.show_cache_warning,
        Some(true),
        "workspace 未设置时不应覆盖全局 true"
    );
}

#[test]
fn test_merge_workspace_explicit_false_overrides_global_true() {
    let mut global = make_global();
    global.show_cache_warning = Some(true);
    let json = r#"{"show_cache_warning":false}"#;
    let workspace: AppConfig = serde_json::from_str(json).unwrap();
    global.merge_overrides(workspace);
    assert_eq!(
        global.show_cache_warning,
        Some(false),
        "workspace 显式 false 应覆盖全局 true"
    );
}

#[test]
fn provider_models_fable_tier_and_fallback() {
    let m = ProviderModels {
        opus: "claude-opus-4-6".into(),
        sonnet: "claude-sonnet-4-6".into(),
        haiku: "claude-haiku-4-5".into(),
        fable: String::new(),
    };
    // fable 档位为空 → 回退 opus
    assert_eq!(m.get_model("fable"), Some("claude-opus-4-6"));
    assert_eq!(m.get_model("FABLE"), Some("claude-opus-4-6"));
    let m2 = ProviderModels {
        fable: "claude-fable-1-0".into(),
        ..m
    };
    assert_eq!(m2.get_model("fable"), Some("claude-fable-1-0"));
    assert_eq!(m2.get_model("opus"), Some("claude-opus-4-6"));
    assert_eq!(m2.get_model("sonnet"), Some("claude-sonnet-4-6"));
    assert_eq!(m2.get_model("haiku"), Some("claude-haiku-4-5"));
    assert_eq!(m2.get_model("turbo"), None);
}

#[test]
fn profile_config_defaults() {
    let p = ProfileConfig::default();
    assert_eq!(p.provider, "");
    assert_eq!(p.model, None);
    assert_eq!(p.effort, "xhigh");
    assert_eq!(p.max_tokens, 32000);
    assert!(!p.context_1m);
}

#[test]
fn profiles_serde_roundtrip_four_tiers() {
    let json = r#"{
        "fable":   { "provider": "a", "effort": "max",   "max_tokens": 64000, "context_1m": true },
        "opus":    { "provider": "a" },
        "sonnet":  {},
        "haiku":   { "provider": "b", "model": "gpt-5.6-luna", "effort": "medium", "max_tokens": 16000, "context_1m": false }
    }"#;
    let profiles: Profiles = serde_json::from_str(json).unwrap();
    assert_eq!(profiles.fable.provider, "a");
    assert_eq!(profiles.fable.effort, "max");
    assert!(profiles.fable.context_1m);
    assert_eq!(profiles.opus.effort, "xhigh"); // 缺省字段用默认
    assert_eq!(profiles.opus.max_tokens, 32000);
    assert_eq!(profiles.haiku.model.as_deref(), Some("gpt-5.6-luna"));
    let back = serde_json::to_value(&profiles).unwrap();
    assert!(
        back.get("fable").is_some()
            && back.get("opus").is_some()
            && back.get("sonnet").is_some()
            && back.get("haiku").is_some()
    );
}

#[test]
fn merge_overrides_profile_whole_replacement() {
    let mut global = AppConfig {
        profiles: Profiles {
            opus: ProfileConfig {
                effort: "high".into(),
                max_tokens: 32000,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ws = AppConfig::default();
    ws.profiles.get_mut("opus").unwrap().effort = "max".into();
    ws.profiles.get_mut("opus").unwrap().max_tokens = 64000;
    global.merge_overrides(ws);
    assert_eq!(global.profiles.opus.effort, "max");
    assert_eq!(global.profiles.opus.max_tokens, 64000);
    // 项目级未定义 fable → 保留全局
    assert_eq!(global.profiles.fable.effort, "xhigh");
}

#[test]
fn serde_deprecated_fields_absorbed_into_extra() {
    let json = r#"{"active_alias":"opus","active_provider_id":"a","thinking":{"enabled":true,"effort":"high"},"context_1m":true,"providers":[]}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.active_alias, "opus");
    assert!(cfg.extra.contains_key("active_provider_id"));
    assert!(cfg.extra.contains_key("thinking"));
    assert!(cfg.extra.contains_key("context_1m"));
}

// ─── meta_harness（设计 §2.1）───────────────────────────────────────────────

fn mh(entries: &[(&str, bool)]) -> HashMap<String, bool> {
    entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn meta_harness_defaults_to_none() {
    let cfg = AppConfig::default();
    assert_eq!(cfg.meta_harness, None);
    // 缺省不改变旧配置解析
    let json = r#"{"active_alias":"haiku"}"#;
    let parsed: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.meta_harness, None);
    // serde roundtrip：None 不序列化该字段
    let value = serde_json::to_value(&cfg).unwrap();
    assert!(!value.as_object().unwrap().contains_key("meta_harness"));
}

#[test]
fn meta_harness_deserializes_bool_entries() {
    let json = r#"{
        "meta_harness": {
            "01_intro": true,
            "05_using_tools": false,
            "WebMiddleware": false,
            "FilesystemMiddleware": true
        }
    }"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    let map = cfg.meta_harness.unwrap();
    assert_eq!(map.get("01_intro"), Some(&true));
    assert_eq!(map.get("05_using_tools"), Some(&false));
    assert_eq!(map.get("WebMiddleware"), Some(&false));
    assert_eq!(map.get("FilesystemMiddleware"), Some(&true));
}

#[test]
fn meta_harness_roundtrip_preserves_values() {
    let cfg = AppConfig {
        meta_harness: Some(mh(&[
            ("01_intro", true),
            ("WebMiddleware", false),
            ("10_hitl", false),
        ])),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.meta_harness, cfg.meta_harness);
}

#[test]
fn meta_harness_merges_per_key() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("01_intro", true), ("WebMiddleware", false)])),
        ..make_global()
    };
    let workspace = AppConfig {
        meta_harness: Some(mh(&[
            ("01_intro", false),           // 同 key：workspace 覆盖 global
            ("TerminalMiddleware", false), // 新 key：保留并追加
        ])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    let map = global.meta_harness.unwrap();
    assert_eq!(
        map.get("01_intro"),
        Some(&false),
        "workspace 同 key 覆盖 global"
    );
    assert_eq!(map.get("TerminalMiddleware"), Some(&false));
    assert_eq!(
        map.get("WebMiddleware"),
        Some(&false),
        "global 其余 key 保留"
    );
}

#[test]
fn meta_harness_false_overrides_true() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("WebMiddleware", true)])),
        ..make_global()
    };
    let workspace = AppConfig {
        meta_harness: Some(mh(&[("WebMiddleware", false)])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(
        global.meta_harness.unwrap().get("WebMiddleware"),
        Some(&false)
    );
}

#[test]
fn meta_harness_true_restores_middleware() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("WebMiddleware", false)])),
        ..make_global()
    };
    let workspace = AppConfig {
        meta_harness: Some(mh(&[("WebMiddleware", true)])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(
        global.meta_harness.unwrap().get("WebMiddleware"),
        Some(&true)
    );
}

#[test]
fn section_false_is_valid() {
    // 段落 ID + false = 显式不覆盖，合法保留（双向语义，设计 §2.1）
    let json = r#"{"meta_harness":{"01_intro":false}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_meta_harness();
    assert_eq!(cfg.meta_harness.unwrap().get("01_intro"), Some(&false));
}

#[test]
fn middleware_true_is_valid() {
    // middleware 名 + true = 显式恢复装配，合法保留
    let json = r#"{"meta_harness":{"WebMiddleware":true}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_meta_harness();
    assert_eq!(cfg.meta_harness.unwrap().get("WebMiddleware"), Some(&true));
}

#[test]
fn unknown_key_warns_and_is_ignored() {
    let json = r#"{"meta_harness":{"NotAMiddleware":false,"01_intro":true}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_meta_harness();
    let map = cfg.meta_harness.unwrap();
    assert!(!map.contains_key("NotAMiddleware"), "未知 key 被移除");
    assert_eq!(map.get("01_intro"), Some(&true), "已知 key 保留");
}

#[test]
fn unknown_key_only_in_workspace_is_ignored_on_merge() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("01_intro", true)])),
        ..make_global()
    };
    // workspace 经 load_from 已校验，未知 key 不存在；merge 本身只做逐 key 覆盖
    let workspace = AppConfig {
        meta_harness: Some(mh(&[("01_intro", false)])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    assert_eq!(global.meta_harness.unwrap().get("01_intro"), Some(&false));
}

#[test]
fn env_still_replaces_whole_map() {
    // 对照：meta_harness 逐 key 合并是专属特例，env 仍整体覆盖
    let mut global = AppConfig {
        env: Some(HashMap::from([("FOO".to_string(), "bar".to_string())])),
        meta_harness: Some(mh(&[("01_intro", true), ("WebMiddleware", false)])),
        ..make_global()
    };
    let workspace = AppConfig {
        env: Some(HashMap::from([("BAZ".to_string(), "qux".to_string())])),
        meta_harness: Some(mh(&[("01_intro", false)])),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    let env = global.env.unwrap();
    assert!(!env.contains_key("FOO"), "env 整体覆盖，global FOO 消失");
    assert_eq!(env.get("BAZ"), Some(&"qux".to_string()));
    let mh_map = global.meta_harness.unwrap();
    assert_eq!(mh_map.get("01_intro"), Some(&false));
    assert_eq!(
        mh_map.get("WebMiddleware"),
        Some(&false),
        "meta_harness 逐 key 合并，global key 保留"
    );
}

#[test]
fn meta_harness_project_none_preserves_global() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("01_intro", true)])),
        ..make_global()
    };
    let workspace = AppConfig::default(); // meta_harness = None
    global.merge_overrides(workspace);
    assert_eq!(global.meta_harness.unwrap().get("01_intro"), Some(&true));
}

#[test]
fn meta_harness_project_empty_does_not_delete_global_keys() {
    let mut global = AppConfig {
        meta_harness: Some(mh(&[("01_intro", true), ("WebMiddleware", false)])),
        ..make_global()
    };
    // workspace 显式空 map：不提供删除语义，global key 全部保留
    let workspace = AppConfig {
        meta_harness: Some(HashMap::new()),
        ..Default::default()
    };
    global.merge_overrides(workspace);
    let map = global.meta_harness.unwrap();
    assert_eq!(map.get("01_intro"), Some(&true));
    assert_eq!(map.get("WebMiddleware"), Some(&false));
}

#[test]
fn meta_harness_empty_map_kept_as_some() {
    // serde roundtrip 不隐式改写用户配置：空 map 保留为 Some(empty)
    let json = r#"{"meta_harness":{}}"#;
    let mut cfg: AppConfig = serde_json::from_str(json).unwrap();
    cfg.validate_meta_harness();
    assert_eq!(cfg.meta_harness, Some(HashMap::new()));
}
