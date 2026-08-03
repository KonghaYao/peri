use super::*;
use peri_middlewares::PermissionMode;

#[test]
fn test_no_overrides_contains_all_sections() {
    let result = build_system_prompt(None, "/tmp", PromptFeatures::none(), &[], None, None);
    assert!(
        result.contains("Following conventions"),
        "应包含 02_system 段落"
    );
    assert!(result.contains("Doing tasks"), "应包含 03_doing_tasks 段落");
    assert!(
        result.contains("Ask Before Diving"),
        "应包含 03_doing_tasks Ask Before Diving 段落"
    );
    assert!(
        result.contains("prefer AskUserQuestion over continuing to guess"),
        "应包含 05_using_tools AskUserQuestion 推测优先提问规则"
    );
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
        mode: None,
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
fn test_workflow_enabled_includes_workflow_section() {
    let features = PromptFeatures {
        workflow_enabled: true,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        result.contains("Workflow Orchestration"),
        "workflow_enabled 时应包含 16_workflow 段落"
    );
}

#[test]
fn test_workflow_disabled_excludes_workflow_section() {
    let features = PromptFeatures {
        workflow_enabled: false,
        ..PromptFeatures::none()
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(
        !result.contains("Workflow Orchestration"),
        "workflow_enabled=false 时不应包含 16_workflow 段落（print mode 等无 executor 场景）"
    );
}

/// [回归测试] workflow 声明与注册共用同一条件源（阶段 3 capability 契约）。
///
/// 历史背景（审计 prompt-sections-audit.md P1-5）：16_workflow 原编入无条件
/// static sections，而 WorkflowTool 注册严格依赖 `workflow_executor.is_some()`
/// ——prompt 宣称与真实注册脱节。修复后 `workflow_enabled` 由同一条件源
/// 导出：prompt section、WorkflowMiddlewareAdaptor::collect_tools、ToolSearch
/// 索引发现三面一致；此测试锁定 prompt 面，注册/发现面分别由
/// workflow/mod.rs 与 tool_search/middleware_test.rs 的用例覆盖。
#[test]
fn test_workflow_gate_marks_capability_contract_layer() {
    // 16_workflow 必须归 CapabilityContract 层（可被 feature 门控，但不得被
    // persona override 移除——它不在 IMMUTABLE_SECTIONS 也不在 PersonaDomain）。
    let (_, gate, layer) = GATED_SECTIONS
        .iter()
        .find(|(s, _, _)| s.contains("Workflow Orchestration"))
        .expect("16_workflow 应位于 GATED_SECTIONS");
    assert_eq!(*gate, FeatureGate::Workflow);
    assert_eq!(*layer, PromptLayer::CapabilityContract);
    // 不可替换层不再包含 workflow 段
    assert!(
        !IMMUTABLE_SECTIONS
            .iter()
            .any(|(s, _)| s.contains("Workflow Orchestration")),
        "16_workflow 不应再位于 IMMUTABLE_SECTIONS"
    );
}

#[test]
fn test_all_features_enabled_includes_all() {
    let features = PromptFeatures {
        hitl_enabled: true,
        subagent_enabled: true,
        skills_enabled: true,
        channel_enabled: true,
        workflow_enabled: true,
    };
    let result = build_system_prompt(None, "/tmp", features, &[], None, None);
    assert!(result.contains("Human-in-the-Loop"), "应包含 HITL 段落");
    assert!(
        result.contains("SubAgent Delegation"),
        "应包含 SubAgent 段落"
    );
    assert!(result.contains("# Skills"), "应包含 Skills 段落标题");
    assert!(result.contains("Channel 频道消息"), "应包含 Channel 段落");
    assert!(
        result.contains("Workflow Orchestration"),
        "应包含 Workflow 段落"
    );
}

#[test]
fn test_detect_default_values() {
    let features = PromptFeatures::detect(PermissionMode::Bypass, true);
    // 默认环境下 hitl_enabled 取决于 permission_mode
    // 注意：Bypass 模式下 hitl_enabled 为 false
    assert!(features.subagent_enabled);
    assert!(features.skills_enabled);
    // ChannelOwner 未装配：channel 不构成运行时能力（P3-2026-08-02）
    assert!(
        !features.channel_enabled,
        "detect() 不得把未装配的 channel 宣称为可用能力"
    );
    // workflow_enabled 来自调用方（workflow_executor.is_some()）
    assert!(features.workflow_enabled);
    assert!(!PromptFeatures::detect(PermissionMode::Bypass, false).workflow_enabled);
}

/// [回归测试] 子 agent / fork / workflow agent 的 prompt features 恒不宣称
/// workflow；未装配的 channel 不作为运行时能力（P2/P3-2026-08-02）。
///
/// 历史背景（P2 pre-commit review）：subagent / fork / workflow agent 三条
/// 路径均传 `shared_tools: None`、无 WorkflowTool，但旧 `features_for_sub`
/// 沿用 `workflow_executor.is_some()`，prompt 仍渲染 16_workflow——与能力
/// 矛盾。`detect_without_workflow` 锁定子面向 prompt 的 workflow 恒关闭；
/// 主 agent 的 workflow 声明仍由调用方显式传入（`detect(mode, true)`）。
///
/// channel 部分（P3）：`ChannelOwner` 未在生产路径装配，旧 `detect()` 硬编码
/// `channel_enabled: true` 使 15_channel 被错误呈现为可用能力；现恒为 false。
/// D6（tag 转义）保持未修复，本测试不宣称其已修复。
#[test]
fn test_detect_without_workflow_and_channel_gates() {
    // 即使主链 workflow 可用，子面向 features 也不得宣称 workflow
    let sub = PromptFeatures::detect_without_workflow(PermissionMode::Default);
    assert!(
        !sub.workflow_enabled,
        "子 agent / fork / workflow agent 的 features 恒不得宣称 workflow"
    );
    assert!(
        !sub.channel_enabled,
        "未装配 ChannelOwner 时 channel 恒不启用"
    );
    // 主 agent 侧（detect 显式传 workflow 可用性）不受影响
    let main = PromptFeatures::detect(PermissionMode::Default, true);
    assert!(main.workflow_enabled, "主 agent 的 workflow 声明保持可用");
    // 渲染面：子面向 features 渲染不出现 16_workflow / 15_channel
    let result = build_system_prompt(None, "/tmp", sub, &[], None, None);
    assert!(
        !result.contains("Workflow Orchestration"),
        "子面向 prompt 不得包含 16_workflow"
    );
    assert!(
        !result.contains("Channel 频道消息"),
        "未装配 channel 时 prompt 不得包含 15_channel"
    );
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
        skills_enabled: true,
        channel_enabled: true,
        workflow_enabled: true,
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
        mode: None,
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
    // D4：catalog 只含 agent_id / tier / access，不注入自由 description
    assert!(
        result.contains("- tester [inherit] [writes]"),
        "Should contain formatted agent entry, got: {}",
        result
    );
    assert!(
        !result.contains("A test agent"),
        "D4: description 不应注入 system prompt，got: {}",
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
        result.contains("- explorer [haiku] [readonly]"),
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
    // D4：不注入 description
    assert!(
        result.contains("- reviewer [inherit] [writes]"),
        "Should contain reviewer entry"
    );
    assert!(
        result.contains("- analyst [inherit] [writes]"),
        "Should contain analyst entry"
    );
    assert!(
        !result.contains("Reviews code") && !result.contains("Analyzes data"),
        "D4: agent description 不应出现在 catalog"
    );
    // Should also contain built-in agents (coder, explorer, general-purpose, plan, verification, web-researcher)
    assert!(
        result.contains("- explorer [haiku] [readonly]"),
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
        result.contains("- explorer [haiku] [readonly]"),
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
        mode: None,
    };
    let empty_overrides = AgentOverrides {
        persona: None,
        tone: None,
        proactiveness: None,
        mode: None,
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
        PromptFeatures::detect(PermissionMode::Bypass, true),
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
    let old = build_system_prompt(
        None,
        "/tmp",
        PromptFeatures::detect(PermissionMode::Bypass, true),
        &[],
        None,
        None,
    );
    let env = PromptEnv::detect("/tmp");
    let new = PromptTemplate::new().render(
        &env,
        &PromptFeatures::detect(PermissionMode::Bypass, true),
        &[],
        None,
    );

    let old_boundary = old.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    let new_boundary = new.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    assert_eq!(
        old_boundary, new_boundary,
        "boundary offset must be identical for Anthropic cache hit"
    );
}

// ─── prompt_mode full / extend tests ─────────────────────────────────────

/// [回归测试] full 模式不再跳过不可替换层。
///
/// 历史背景：`prompt_mode: full` 曾跳过全部 STATIC_SECTIONS（01-06, 16），
/// 使 subagent 定义可移除防御性安全、secret 规则、Git guardrails 与基础工具纪律
/// （审计 docs/design/prompt-sections-audit.md P0-1）。分层重构后 full 只替换
/// PersonaDomain 层，不可替换层必须保留。
#[test]
fn test_render_full_mode_preserves_immutable_layers() {
    let overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    // 不可替换层保留（SafetyAuthorization / EngineeringBehavior / CapabilityContract）
    assert!(
        result.contains("Following conventions"),
        "full 模式不应移除 02_system 段落"
    );
    assert!(
        result.contains("Doing tasks"),
        "full 模式不应移除 03_doing_tasks 段落"
    );
    assert!(
        result.contains("Simplicity"),
        "full 模式不应移除 04_actions 段落"
    );
    // persona 替换生效（PersonaDomain 层被 full body 替换）
    assert!(
        result.contains("You are a custom full-mode agent."),
        "full 模式应包含 persona 作为 PersonaDomain 层"
    );
}

/// [回归测试] full 模式必须保留 secret 处理规则。
///
/// 历史背景：`full` 曾跳过 02_system.md 的 secret 防泄漏规则
/// （审计 docs/design/prompt-sections-audit.md P0-1）。
#[test]
fn test_render_full_mode_preserves_secret_policy() {
    let overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("Treat secrets"),
        "full 模式不得移除 secret 处理规则（02_system）"
    );
}

/// [回归测试] full 模式必须保留 Git 安全协议。
///
/// 历史背景：`full` 曾跳过 04_actions.md 的 Git Safety Protocol
/// （审计 docs/design/prompt-sections-audit.md P0-1）。
#[test]
fn test_render_full_mode_preserves_git_guardrails() {
    let overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("NEVER force-push to main/master"),
        "full 模式不得移除 Git 安全协议（04_actions）"
    );
}

/// [回归测试] full 模式必须保留基础工具纪律。
///
/// 历史背景：`full` 曾跳过 05_using_tools.md 的工具调用纪律
/// （审计 docs/design/prompt-sections-audit.md P0-1）。
#[test]
fn test_render_full_mode_preserves_tool_discipline() {
    let overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("Tool usage policy"),
        "full 模式不得移除工具纪律段落（05_using_tools）"
    );
    assert!(
        result.contains("## Bash discipline"),
        "full 模式不得移除 Bash 纪律段落（05_using_tools）"
    );
}

/// full 模式的 boundary 前缀偏移必须与 extend 模式完全一致。
///
/// 分层后 full 模式同样渲染不可替换层，边界标记前字节与非 full 相同，
/// 恢复 Anthropic 前缀缓存命中区域的一致性。
#[test]
fn test_render_full_mode_boundary_aligned_with_extend() {
    let full_overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let full = build_system_prompt(
        Some(&full_overrides),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    let extend = build_system_prompt(
        None,
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    let full_boundary = full.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    let extend_boundary = extend.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    assert_eq!(
        full_boundary, extend_boundary,
        "full/extend 的 boundary 偏移应一致"
    );
    assert_eq!(
        &full[..full_boundary],
        &extend[..extend_boundary],
        "boundary 之前的不可替换层字节应一致"
    );
}

/// 验证固定层顺序：SafetyAuthorization → EngineeringBehavior → BOUNDARY →
/// PersonaDomain → RuntimeStateBoundary → gated sections（含 capability 契约）。
///
/// 16_workflow 属 CapabilityContract 层，但作为 gated section 渲染在
/// boundary 之后（FeatureGate::Workflow 控制可见性，见阶段 3 capability 契约）。
#[test]
fn test_render_immutable_layer_order() {
    // frozen_date 参数化，避免触发 chrono::Local::now()（testing-standards 4.1 确定性）
    let features = PromptFeatures {
        hitl_enabled: true,
        subagent_enabled: true,
        skills_enabled: true,
        channel_enabled: true,
        workflow_enabled: true,
    };
    let result = build_system_prompt(None, "/tmp", features, &[], Some("2026-01-01"), None);
    let safety_pos = result.find("Treat secrets").unwrap(); // 02_system（SafetyAuthorization）
    let engineering_pos = result.find("# Doing tasks").unwrap(); // 03_doing_tasks（EngineeringBehavior）
    let boundary_pos = result.find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__").unwrap();
    let runtime_pos = result.find("<env>").unwrap(); // 07_env（RuntimeStateBoundary）
    let capability_pos = result.find("Workflow Orchestration").unwrap(); // 16_workflow（CapabilityContract）
    assert!(
        safety_pos < engineering_pos,
        "SafetyAuthorization 层应位于 EngineeringBehavior 层之前"
    );
    assert!(
        engineering_pos < boundary_pos,
        "不可替换层（工程行为）应位于边界标记之前"
    );
    assert!(
        boundary_pos < runtime_pos,
        "RuntimeStateBoundary 层应位于边界标记之后"
    );
    assert!(
        boundary_pos < capability_pos,
        "gated capability 段（16_workflow）应位于边界标记之后（feature 门控）"
    );
}

/// 验证 full 模式下保留 env 动态段（07）
#[test]
fn test_render_full_mode_keeps_env() {
    let overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        Some(&overrides),
        "/custom/project",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    // 动态段 env (07) 应保留
    assert!(
        result.contains("<env>"),
        "full 模式应保留 07_env 环境信息段落"
    );
    assert!(
        result.contains("/custom/project"),
        "full 模式下 cwd 占位符应被替换"
    );
}

/// 验证 extend 模式（mode=None 与 mode=Some("extend")）行为一致，输出完全相同
#[test]
fn test_render_extend_mode_unchanged() {
    let overrides_none = AgentOverrides {
        persona: Some("You are a test agent.".into()),
        tone: Some("Be concise".into()),
        proactiveness: None,
        mode: None,
    };
    let overrides_extend = AgentOverrides {
        persona: Some("You are a test agent.".into()),
        tone: Some("Be concise".into()),
        proactiveness: None,
        mode: Some("extend".into()),
    };
    let result_none = build_system_prompt(
        Some(&overrides_none),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    let result_extend = build_system_prompt(
        Some(&overrides_extend),
        "/tmp",
        PromptFeatures::none(),
        &[],
        Some("2026-01-01"),
        None,
    );
    // 两种方式输出应完全一致
    assert_eq!(
        result_none, result_extend,
        "extend 模式下 mode=None 与 mode=Some(\"extend\") 应产生相同输出"
    );
    // 静态段应包含
    assert!(
        result_none.contains("Following conventions"),
        "extend 模式应包含静态段"
    );
}

// ─── P2: Git 仓库上溯探测测试 ─────────────────────────────────────────────

/// [回归测试] P2-12：Git 探测向上查找，仓库子目录不再误判为非仓库。
///
/// 历史背景（审计 prompt-sections-audit.md P2-12）：旧判定只检查
/// `cwd/.git`，在 monorepo 子目录（packages/foo）启动会话会被误标为非仓库，
/// 与 `git` 命令的上溯发现语义不一致。
#[test]
fn test_detect_is_git_repo_in_subdirectory() {
    let dir = tmp_dir("prompt_test_git_subdir");
    // 仓库根在 dir，子目录 dir/packages/foo
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let sub = dir.join("packages").join("foo");
    std::fs::create_dir_all(&sub).unwrap();

    assert!(
        detect_is_git_repo(dir.to_str().unwrap()),
        "仓库根应判定为 Git"
    );
    assert!(
        detect_is_git_repo(sub.to_str().unwrap()),
        "仓库子目录应向上查找到 .git"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// [回归测试] P2-12：`.git` 为文件（worktree / submodule）时同样判定为仓库。
#[test]
fn test_detect_is_git_repo_with_git_file_worktree() {
    let dir = tmp_dir("prompt_test_git_file");
    std::fs::write(dir.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();

    assert!(
        detect_is_git_repo(dir.to_str().unwrap()),
        ".git 文件（worktree/submodule）也应判定为 Git 仓库"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 非仓库目录（含嵌套目录）判定为非仓库。
#[test]
fn test_detect_is_git_repo_non_repo() {
    let dir = tmp_dir("prompt_test_git_nonrepo");
    let nested = dir.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();

    assert!(
        !detect_is_git_repo(dir.to_str().unwrap()),
        "无 .git 的目录不应判定为仓库"
    );
    assert!(
        !detect_is_git_repo(nested.to_str().unwrap()),
        "无 .git 的嵌套目录不应判定为仓库"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
