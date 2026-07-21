use super::*;

#[test]
fn test_no_overrides_contains_all_sections() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        result.contains("Following conventions"),
        "应包含 02_system 段落"
    );
    assert!(result.contains("Doing tasks"), "应包含 03_doing_tasks 段落");
    assert!(result.contains("<env>"), "应包含 07_env 段落");
    assert!(
        result.contains("Working directory"),
        "应包含 08_env 替换后结果"
    );
}

#[test]
fn test_no_overrides_no_duplicate_tone_proactiveness() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    // "# Tone and style" 仅出现 1 次（来自 06_tone_style.md 静态段落，不来自覆盖块）
    assert_eq!(
        result.matches("# Tone and style").count(),
        1,
        "无 overrides 时 # Tone and style 应仅出现 1 次（来自静态段落）"
    );
    // "# Proactiveness" 仅出现 1 次（来自 02_system.md 静态段落）
    assert_eq!(
        result.matches("# Proactiveness").count(),
        1,
        "无 overrides 时 # Proactiveness 应仅出现 1 次（来自静态段落）"
    );
    // "Simplicity" 出现在 04_actions.md
    assert!(
        result.contains("Simplicity"),
        "应包含 04_actions Simplicity 段落"
    );
}

#[test]
fn test_no_overrides_no_leading_newlines() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        !result.starts_with("\n\n"),
        "无 overrides 时提示词不应以空行开头"
    );
}

#[test]
fn test_with_overrides_uses_override_block() {
    let overrides = AgentOverrides {
        persona: Some("test persona".into()),
        tone: None,
        proactiveness: None,
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        None,
        None,
    );
    // overrides 现在在边界标记之后，不再以 persona 开头
    let boundary = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    assert!(
        result[boundary..].contains("test persona"),
        "有 overrides 时边界之后应包含 persona 内容"
    );
    // 静态段应在 persona 之前（边界标记之前）
    assert!(
        !result[..boundary].contains("test persona"),
        "persona 不应在缓存段内"
    );
}

#[test]
fn test_placeholders_replaced() {
    let result = build_system_prompt(
        None,
        "/custom/path",
        PromptFeatures::none(),
        &[],
        None,
        None,
    );
    assert!(!result.contains("{{"), "不应包含未替换的占位符");
    assert!(result.contains("/custom/path"), "cwd 占位符应被替换");
}

#[test]
fn test_env_contains_cwd() {
    let result = build_system_prompt(
        None,
        "/custom/path",
        PromptFeatures::none(),
        &[],
        None,
        None,
    );
    assert!(result.contains("/custom/path"), "环境信息应包含 cwd");
}

#[test]
fn test_features_none_excludes_all_gated_sections() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        !result.contains("Human-in-the-Loop"),
        "全关闭时不应包含 HITL 段落"
    );
    assert!(
        !result.contains("SubAgent Delegation"),
        "全关闭时不应包含 SubAgent 段落"
    );
    assert!(
        !result.contains("Scheduled Tasks"),
        "全关闭时不应包含 Cron 段落"
    );
    // 13_skills.md 以 "# Skills\n" 开头，检查标题
    assert!(
        !result.contains("\n# Skills\n") && !result.starts_with("# Skills\n"),
        "全关闭时不应包含 Skills 标题段落"
    );
    assert!(
        !result.contains("Channel 频道消息"),
        "全关闭时不应包含 Channel 段落"
    );
}

#[test]
fn test_hitl_enabled_includes_hitl_section() {
    let features = PromptFeatures {
        hitl_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        result.contains("Human-in-the-Loop"),
        "hitl_enabled 时应包含 HITL 段落"
    );
}

#[test]
fn test_subagent_enabled_includes_subagent_section() {
    let features = PromptFeatures {
        subagent_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        result.contains("SubAgent Delegation"),
        "subagent_enabled 时应包含 SubAgent 段落"
    );
}

#[test]
fn test_cron_enabled_includes_cron_section() {
    let features = PromptFeatures {
        cron_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        result.contains("Scheduled Tasks"),
        "cron_enabled 时应包含 Cron 段落"
    );
}

#[test]
fn test_skills_enabled_includes_skills_section() {
    let features = PromptFeatures {
        skills_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        result.contains("# Skills"),
        "skills_enabled 时应包含 Skills 段落标题"
    );
}

#[test]
fn test_all_features_enabled_includes_all() {
    let features = PromptFeatures {
        hitl_enabled: true,
        subagent_enabled: true,
        cron_enabled: true,
        skills_enabled: true,
        channel_enabled: true,
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(result.contains("Human-in-the-Loop"), "应包含 HITL 段落");
    assert!(
        result.contains("SubAgent Delegation"),
        "应包含 SubAgent 段落"
    );
    assert!(result.contains("Scheduled Tasks"), "应包含 Cron 段落");
    assert!(result.contains("# Skills"), "应包含 Skills 段落标题");
    assert!(result.contains("Channel 频道消息"), "应包含 Channel 段落");
}

#[test]
fn test_detect_default_values() {
    let features = PromptFeatures::detect();
    // 默认环境（无 YOLO_MODE 或 YOLO_MODE=true）下 hitl_enabled 为 false
    // 注意：测试环境中 YOLO_MODE 可能未设置
    assert!(features.subagent_enabled);
    assert!(features.cron_enabled);
    assert!(features.skills_enabled);
    assert!(features.channel_enabled);
}

// ─── boundary marker tests ──────────────────────────────────────────────

#[test]
fn test_boundary_marker_present() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        result.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"),
        "system prompt 应包含边界标记"
    );
}

#[test]
fn test_boundary_marker_before_dynamic_content() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    // 06_tone_style 在边界之前
    assert!(
        result[..boundary_pos].contains("# Tone and style"),
        "06_tone_style 应在边界标记之前"
    );
    // 07_env 在边界之后
    assert!(
        result[boundary_pos..].contains("Working directory"),
        "07_env 应在边界标记之后"
    );
}

#[test]
fn test_boundary_marker_with_all_features() {
    let features = PromptFeatures {
        hitl_enabled: true,
        subagent_enabled: true,
        cron_enabled: true,
        skills_enabled: true,
        channel_enabled: true,
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    // feature-gated 段落都应在边界之后
    assert!(
        result[boundary_pos..].contains("Human-in-the-Loop"),
        "HITL 段落应在边界标记之后"
    );
    assert!(
        result[boundary_pos..].contains("SubAgent Delegation"),
        "SubAgent 段落应在边界标记之后"
    );
}

#[test]
fn test_overrides_after_boundary_marker() {
    let overrides = AgentOverrides {
        persona: Some("test persona".into()),
        tone: Some("concise".into()),
        proactiveness: None,
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        None,
        None,
    );
    let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    // overrides 应在边界之后，不破坏缓存前缀
    assert!(
        result[boundary_pos..].contains("test persona"),
        "persona 应在边界标记之后"
    );
    assert!(
        result[boundary_pos..].contains("concise"),
        "tone 应在边界标记之后"
    );
    // 边界之前不应包含 overrides 内容
    assert!(
        !result[..boundary_pos].contains("test persona"),
        "persona 不应在边界标记之前（会破坏缓存前缀）"
    );
}

// ─── available_agents tests ──────────────────────────────────────────────

/// Helper: create a unique temp directory under /tmp
fn tmp_dir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_available_agents_placeholder_replaced() {
    let dir = tmp_dir("prompt_test_agent_replaced");
    let agents_dir = dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("tester.md"),
        "---\nname: tester\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let features = PromptFeatures {
        subagent_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, dir.to_str().unwrap(), features, &[], None, None);
    assert!(
        result.contains("- tester [inherit] [writes]: A test agent"),
        "Should contain formatted agent entry, got: {}",
        result
    );
    assert!(
        !result.contains("{{available_agents}}"),
        "Placeholder should be replaced"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_available_agents_placeholder_empty_dir() {
    let dir = tmp_dir("prompt_test_agent_empty");
    // No .claude/agents/ directory at all
    let features = PromptFeatures {
        subagent_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, dir.to_str().unwrap(), features, &[], None, None);
    assert!(
        result.contains("- explorer [haiku] [readonly]:"),
        "Should contain built-in agents even without .claude/agents/ directory"
    );
    assert!(
        !result.contains("No agents currently configured"),
        "Should NOT show no-agents message when built-in agents exist"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_available_agents_not_replaced_when_subagent_disabled() {
    let dir = tmp_dir("prompt_test_agent_disabled");
    let features = PromptFeatures::none();
    let result = build_system_prompt(None, dir.to_str().unwrap(), features, &[], None, None);
    assert!(
        !result.contains("SubAgent Delegation"),
        "SubAgent section should not be included when disabled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_format_available_agents_with_agents() {
    let dir = tmp_dir("prompt_test_format_agents");
    let agents_dir = dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews code\n---\n\nReview code.\n",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("analyst.md"),
        "---\nname: data-analyst\ndescription: Analyzes data\n---\n\nAnalyze data.\n",
    )
    .unwrap();

    let result = format_available_agents(dir.to_str().unwrap(), &[]);
    assert!(
        result.contains("- reviewer [inherit] [writes]: Reviews code"),
        "Should contain reviewer entry"
    );
    assert!(
        result.contains("- analyst [inherit] [writes]: Analyzes data"),
        "Should contain analyst entry"
    );
    // Should also contain built-in agents (coder, explorer, general-purpose, plan, verification, web-researcher)
    assert!(
        result.contains("- explorer [haiku] [readonly]:"),
        "Should contain built-in explorer agent"
    );
    // Verify project agents + built-in agents
    let lines: Vec<&str> = result.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(
        lines.len(),
        8,
        "Should have 2 project + 6 built-in agent entries"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_format_available_agents_empty_dir() {
    let result = format_available_agents("/nonexistent/path/that/does/not/exist", &[]);
    // Built-in agents are always available
    assert!(
        result.contains("- explorer [haiku] [readonly]:"),
        "Should contain built-in agents even without .claude/agents/ directory"
    );
    assert!(
        !result.contains("No agents currently configured"),
        "Should NOT show no-agents message when built-in agents exist"
    );
}

// ─── language injection tests ───────────────────────────────────────────

#[test]
fn test_language_simplified_chinese_injected() {
    let result = build_system_prompt(
        None,
        "/tmp",
        PromptFeatures::none(),
        &[],
        None,
        Some("zh-CN"),
    );
    assert!(
        result.contains("# Language"),
        "language=zh-CN 时应包含 # Language 标题"
    );
    assert!(
        result.contains("Simplified Chinese"),
        "zh-CN 应映射到 Simplified Chinese"
    );
    assert!(
        result
            .contains("Technical terms and code identifiers should remain in their original form"),
        "应包含技术术语保留原文指示"
    );
}

#[test]
fn test_language_none_no_injection() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        !result.contains("\n# Language\n"),
        "language=None 时不应注入 Language 段落"
    );
}

#[test]
fn test_language_section_after_boundary_marker() {
    let result = build_system_prompt(
        None,
        "/tmp",
        PromptFeatures::none(),
        &[],
        None,
        Some("zh-CN"),
    );
    let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    assert!(
        result[boundary_pos..].contains("# Language"),
        "Language 段落应在边界标记之后（动态区域，不破坏缓存前缀）"
    );
    assert!(
        !result[..boundary_pos].contains("# Language"),
        "Language 段落不应在边界标记之前（会破坏缓存前缀）"
    );
}

#[test]
fn test_language_zh_maps_to_simplified_chinese() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, Some("zh"));
    assert!(
        result.contains("Simplified Chinese"),
        "zh 应映射到 Simplified Chinese"
    );
}

#[test]
fn test_language_custom_code_passthrough() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, Some("fr"));
    assert!(
        result.contains("Always respond in fr"),
        "未知语言代码应原样保留"
    );
}

// ─── snapshot tests ───────────────────────────────────────────────────────

/// 验证 PromptTemplate::render() 与 build_system_prompt() 输出字节完全一致
/// [回归测试] 确保 PromptTemplate 重构不改变系统提示词字节
#[test]
fn test_prompt_template_byte_identical_to_build_system_prompt() {
    let frozen_date = "2026-01-01";
    let cwd = "/test/project";
    let no_overrides: Option<&AgentOverrides> = None;
    let with_overrides = AgentOverrides {
        persona: Some("You are a test bot".into()),
        tone: Some("Be concise".into()),
        proactiveness: Some("Ask before acting".into()),
    };
    let empty_overrides = AgentOverrides {
        persona: None,
        tone: None,
        proactiveness: None,
    };

    // 覆盖多种 features 组合
    let features_combos = [
        PromptFeatures::none(),
        {
            let mut f = PromptFeatures::none();
            f.subagent_enabled = true;
            f
        },
        {
            let mut f = PromptFeatures::none();
            f.hitl_enabled = true;
            f
        },
        {
            let mut f = PromptFeatures::none();
            f.skills_enabled = true;
            f
        },
        PromptFeatures::detect(),
    ];

    let language_combos: [Option<&str>; 3] = [None, Some("zh-CN"), Some("fr")];

    for features in &features_combos {
        for language in &language_combos {
            // No overrides
            {
                let old = build_system_prompt(
                    no_overrides,
                    cwd,
                    *features,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let new = PromptTemplate::new().render(&env, features, &[], *language);
                assert_eq!(
                    old, new,
                    "byte mismatch: features={:?}, lang={:?}, overrides=None",
                    features, language
                );
            }
            // With non-empty overrides
            {
                let old = build_system_prompt(
                    Some(&with_overrides),
                    cwd,
                    *features,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let new = PromptTemplate::with_overrides(&with_overrides).render(
                    &env,
                    features,
                    &[],
                    *language,
                );
                assert_eq!(
                    old, new,
                    "byte mismatch: features={:?}, lang={:?}, overrides=Some",
                    features, language
                );
            }
            // With empty overrides (should behave same as None)
            {
                let old = build_system_prompt(
                    Some(&empty_overrides),
                    cwd,
                    *features,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let new = PromptTemplate::with_overrides(&empty_overrides).render(
                    &env,
                    features,
                    &[],
                    *language,
                );
                assert_eq!(
                    old, new,
                    "byte mismatch: features={:?}, lang={:?}, overrides=Some(empty)",
                    features, language
                );
            }
        }
    }
}

/// 验证边界标记位置在新旧路径中完全一致
#[test]
fn test_template_boundary_position_identical() {
    let old = build_system_prompt(None, "/tmp", PromptFeatures::detect(), &[], None, None);
    let env = PromptEnv::detect("/tmp");
    let new = PromptTemplate::new().render(&env, &PromptFeatures::detect(), &[], None);

    let old_boundary = old.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    let new_boundary = new.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    assert_eq!(
        old_boundary, new_boundary,
        "boundary offset must be identical for Anthropic cache hit"
    );
}
