use super::*;

// ── ThinkingConfig::openai_effort ─────────────────────────────────────────

#[test]
fn test_thinking_effort_direct() {
    let c = ThinkingConfig {
        enabled: true,
        budget_tokens: 0,
        effort: "low".to_string(),
        max_tokens: 32000,
    };
    assert_eq!(c.openai_effort(), "low");
}

// next_effort / prev_effort removed — TUI-only dead code// ── ThinkingConfig 序列化 / 反序列化 ─────────────────────────────────────

#[test]
fn test_thinking_config_serde_roundtrip() {
    let cfg = ThinkingConfig {
        enabled: true,
        budget_tokens: 5000,
        effort: "medium".to_string(),
        max_tokens: 32000,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ThinkingConfig = serde_json::from_str(&json).unwrap();
    assert!(back.enabled);
    assert_eq!(back.budget_tokens, 5000);
    assert_eq!(back.effort, "medium");
    assert_eq!(back.max_tokens, 32000);
}

#[test]
fn test_thinking_config_default_budget() {
    // 不传 budget_tokens 时应默认 8000，effort 默认 medium
    let json = r#"{"enabled": false}"#;
    let cfg: ThinkingConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.budget_tokens, 8000);
    assert_eq!(cfg.max_tokens, 32000);
}

#[test]
fn test_app_config_thinking_optional() {
    // thinking 字段缺失时应为 None（使用新格式字段）
    let json = r#"{"active_alias": "opus", "active_provider_id": "", "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.thinking.is_none());
}

#[test]
fn test_app_config_thinking_roundtrip() {
    let json = r#"{
            "active_alias": "opus",
            "providers": [],
            "thinking": {"enabled": true, "budget_tokens": 8000}
        }"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    let t = cfg.thinking.as_ref().unwrap();
    assert!(t.enabled);
    assert_eq!(t.budget_tokens, 8000);

    // 序列化后 thinking 字段存在
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(out.contains("\"thinking\""));
    // active_alias 字段正确序列化
    assert!(out.contains("\"active_alias\""));
}

#[test]
fn test_app_config_thinking_skip_when_none() {
    let cfg = AppConfig::default(); // thinking = None
    let out = serde_json::to_string(&cfg).unwrap();
    // skip_serializing_if = "Option::is_none"，所以 thinking 字段不应出现
    assert!(
        !out.contains("thinking"),
        "thinking should be absent when None"
    );
}

// ── ModelPanel thinking 缓冲逻辑（已迁移至 model_panel.rs）─────────────────

// ── ProviderModels 测试 ───────────────────────────────────────────────────

#[test]
fn test_provider_models_get_model_known_aliases() {
    let models = ProviderModels {
        opus: "o".to_string(),
        sonnet: "s".to_string(),
        haiku: "h".to_string(),
    };
    assert_eq!(models.get_model("opus"), Some("o"));
    assert_eq!(models.get_model("sonnet"), Some("s"));
    assert_eq!(models.get_model("haiku"), Some("h"));
}

#[test]
fn test_provider_models_get_model_case_insensitive() {
    let models = ProviderModels {
        opus: "o".to_string(),
        sonnet: "s".to_string(),
        haiku: "h".to_string(),
    };
    assert_eq!(models.get_model("Opus"), Some("o"));
    assert_eq!(models.get_model("SONNET"), Some("s"));
    assert_eq!(models.get_model("Haiku"), Some("h"));
}

#[test]
fn test_provider_models_get_model_unknown_returns_none() {
    let models = ProviderModels {
        opus: "o".to_string(),
        sonnet: "s".to_string(),
        haiku: "h".to_string(),
    };
    assert_eq!(models.get_model("turbo"), None);
}

#[test]
fn test_provider_models_default() {
    let models = ProviderModels::default();
    assert!(models.opus.is_empty());
    assert!(models.sonnet.is_empty());
    assert!(models.haiku.is_empty());
}

#[test]
fn test_provider_config_models_serde_roundtrip() {
    let p = ProviderConfig {
        id: "test".to_string(),
        provider_type: "anthropic".to_string(),
        api_key: "key".to_string(),
        base_url: String::new(),
        name: Some("Test".to_string()),
        models: ProviderModels {
            opus: "claude-opus-4-7".to_string(),
            sonnet: "claude-sonnet-4-6".to_string(),
            haiku: "claude-haiku-4-5".to_string(),
        },
        thinking: None,
        extra: Default::default(),
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: ProviderConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.models.opus, "claude-opus-4-7");
    assert_eq!(back.models.sonnet, "claude-sonnet-4-6");
    assert_eq!(back.models.haiku, "claude-haiku-4-5");
}

#[test]
fn test_app_config_active_provider_id_serde() {
    let json = r#"{"active_alias": "opus", "active_provider_id": "anthropic", "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.active_provider_id, "anthropic");
}

#[test]
fn test_app_config_old_fields_ignored() {
    let json = r#"{"provider_id": "old", "model_id": "old-model", "model_aliases": {"opus": {"provider_id": "x", "model_id": "y"}}, "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    // 旧字段被 extra 吸收，active_provider_id 为默认空字符串
    assert_eq!(cfg.active_provider_id, "");
}

// ── AppConfig env 字段测试 ─────────────────────────────────────────────────

#[test]
fn test_app_config_env_serde_roundtrip() {
    let mut env = std::collections::HashMap::new();
    env.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-123".to_string());
    env.insert("RUST_LOG".to_string(), "debug".to_string());

    let cfg = AppConfig {
        env: Some(env),
        ..Default::default()
    };

    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();

    assert!(back.env.is_some());
    let env_back = back.env.unwrap();
    assert_eq!(
        env_back.get("ANTHROPIC_API_KEY"),
        Some(&"sk-ant-123".to_string())
    );
    assert_eq!(env_back.get("RUST_LOG"), Some(&"debug".to_string()));
}

#[test]
fn test_app_config_env_optional() {
    // env 字段缺失时应为 None
    let json = r#"{"active_alias": "opus", "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.env.is_none());
}

#[test]
fn test_app_config_env_skip_when_none() {
    let cfg = AppConfig::default(); // env = None
    let out = serde_json::to_string(&cfg).unwrap();
    // skip_serializing_if = "Option::is_none"，所以 env 字段不应出现
    assert!(!out.contains("env"), "env should be absent when None");
}

// ── AppConfig compact 字段测试 ─────────────────────────────────────────────

#[test]
fn test_app_config_compact_serde_roundtrip() {
    let compact = peri_agent::agent::CompactConfig {
        auto_compact_enabled: false,
        auto_compact_threshold: 0.9,
        ..Default::default()
    };
    let cfg = AppConfig {
        compact: Some(compact),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    let c = back.compact.unwrap();
    assert!(!c.auto_compact_enabled);
    assert!((c.auto_compact_threshold - 0.9).abs() < 0.001);
}

#[test]
fn test_app_config_compact_none_when_absent() {
    let json = r#"{"active_alias": "opus", "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.compact.is_none());
}

#[test]
fn test_app_config_compact_skip_when_none() {
    let cfg = AppConfig::default();
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(
        !out.contains("compact"),
        "compact should be absent when None"
    );
}

// ── AppConfig new fields (language/persona/tone/proactiveness) ──────────

#[test]
fn test_app_config_new_fields_optional() {
    let json = r#"{"active_alias": "opus", "providers": []}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.language.is_none());
    assert!(cfg.persona.is_none());
    assert!(cfg.tone.is_none());
    assert!(cfg.proactiveness.is_none());
}

#[test]
fn test_app_config_language_serde_roundtrip() {
    let cfg = AppConfig {
        language: Some("zh-CN".to_string()),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.language.as_deref(), Some("zh-CN"));
}

#[test]
fn test_app_config_proactiveness_serde_roundtrip() {
    let cfg = AppConfig {
        proactiveness: Some("low".to_string()),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.proactiveness.as_deref(), Some("low"));
}

#[test]
fn test_app_config_persona_tone_skip_when_none() {
    let cfg = AppConfig::default();
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(
        !out.contains("persona"),
        "persona should be absent when None"
    );
    assert!(!out.contains("tone"), "tone should be absent when None");
}

// ── PeriConfig $schema passthrough ──────────────────────────────────────

#[test]
fn test_peri_config_schema_roundtrip() {
    let json = r#"{ "$schema": "https://example.com/schema.json", "config": {} }"#;
    let cfg: PeriConfig = serde_json::from_str(json).unwrap();
    assert_eq!(
        cfg.schema.as_deref(),
        Some("https://example.com/schema.json")
    );
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(out.contains("$schema"));
}

#[test]
fn test_peri_config_schema_none_absent() {
    let cfg = PeriConfig::default();
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(!out.contains("$schema"));
}

// ── AppConfig claude_md_excludes ────────────────────────────────────────

#[test]
fn test_app_config_claude_md_excludes_none_absent() {
    let cfg = AppConfig::default();
    let out = serde_json::to_string(&cfg).unwrap();
    assert!(
        !out.contains("claude_md_excludes"),
        "claude_md_excludes should be absent when None"
    );
}

#[test]
fn test_app_config_claude_md_excludes_roundtrip() {
    let cfg = AppConfig {
        claude_md_excludes: Some(vec!["node_modules/**".to_string()]),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.claude_md_excludes,
        Some(vec!["node_modules/**".to_string()])
    );
}
