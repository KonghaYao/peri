use super::*;

#[test]
fn test_overrides_after_boundary_marker() {
    let overrides = AgentOverrides {
        persona: Some("test persona".into()),
        tone: Some("concise".into()),
        proactiveness: None,
        mode: None,
    };
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    // Persona 段（persona/tone/proactiveness）在缓存区段之后、07_runtime
    // 之前（位置属性负责装配，transport token 把 cache seam 交给 provider）
    let pos_tone_style = result.find("# Tone and style").unwrap();
    assert!(
        result[pos_tone_style..].contains("test persona"),
        "persona 应在缓存区段之后"
    );
    assert!(
        result[pos_tone_style..].contains("concise"),
        "tone 应在缓存区段之后"
    );
    // 缓存区段（01-06）不应包含 overrides 内容
    assert!(
        !result[..pos_tone_style].contains("test persona"),
        "persona 不应在缓存区段内（会破坏缓存前缀）"
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

    let features = PromptFeatures::none();
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        dir.to_str().unwrap(),
        features,
        &SkillsProvider,
        &[],
        None,
        None,
    );
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
    let features = PromptFeatures::none();
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        dir.to_str().unwrap(),
        features,
        &SkillsProvider,
        &[],
        None,
        None,
    );
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

/// C3（gate 原子迁移）：11_subagent 由 SubAgentMiddleware 持有，收集段恒
/// 渲染（持有者装配即渲染）——`{{available_agents}}` 占位符在渲染层替换；
/// 关闭持有者（disabled_middlewares）时段落整体消失（见
/// `meta_harness_disabling_holder_removes_section`）。
#[test]
fn test_available_agents_replaced_when_subagent_holder_enabled() {
    let dir = tmp_dir("prompt_test_agent_holder");
    let features = PromptFeatures::none();
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        dir.to_str().unwrap(),
        features,
        &SkillsProvider,
        &[],
        None,
        None,
    );
    assert!(
        result.contains("SubAgent Delegation"),
        "11_subagent 段落应由持有者装配渲染"
    );
    assert!(
        !result.contains("{{available_agents}}"),
        "catalog 占位符应在渲染层替换"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 盲区闭合（任务 4）：关闭 SubAgentMiddleware → 11_subagent 段落消失
/// （渲染面 crate::session::build_collected_sections 冻结 disabled 集合驱动过滤）。
#[test]
fn meta_harness_disabling_holder_removes_section() {
    let mut state = MetaHarnessState::default();
    state
        .disabled_middlewares
        .insert("SubAgentMiddleware".to_string());
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(
        !result.contains("SubAgent Delegation"),
        "关闭 SubAgentMiddleware 后 11_subagent 段落应消失（盲区闭合）"
    );
    assert!(
        result.contains("Human-in-the-Loop"),
        "其他持有者段落不受影响（10_hitl 仍在）"
    );
    assert!(
        result.contains("# Skills"),
        "其他持有者段落不受影响（13_skills 仍在）"
    );
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

    let result = format_available_agents(&SkillsProvider, dir.to_str().unwrap(), &[], true);
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
    let result = format_available_agents(
        &SkillsProvider,
        "/nonexistent/path/that/does/not/exist",
        &[],
        true,
    );
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
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
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
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    assert!(
        !result.contains("\n# Language\n"),
        "language=None 时不应注入 Language 段落"
    );
}

#[test]
fn test_language_section_after_dynamic_content() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        Some("zh-CN"),
    );
    // Language 段在非缓存区最后（07_runtime 之后；language 由
    // LangMiddleware 持有，整体位于 cache boundary transport token 后）
    let pos_runtime = result.find("## System Reminders").unwrap();
    assert!(
        result[pos_runtime..].contains("# Language"),
        "Language 段落应在 07_runtime 之后（动态区域，不破坏缓存前缀）"
    );
    let pos_tone = result.find("# Tone and style").unwrap();
    assert!(
        !result[..pos_tone].contains("# Language"),
        "Language 段落不应在缓存区段内（会破坏缓存前缀）"
    );
}

#[test]
fn test_language_zh_maps_to_simplified_chinese() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        Some("zh"),
    );
    assert!(
        result.contains("Simplified Chinese"),
        "zh 应映射到 Simplified Chinese"
    );
}

#[test]
fn test_language_custom_code_passthrough() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        Some("fr"),
    );
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

    // 覆盖多种 features 组合（C3：gate 判定仅剩 channel——收集段恒渲染，
    // 组合保持以验证两条渲染路径在所有 gate 配置下字节一致）
    let features_combos = [
        PromptFeatures::none(),
        PromptFeatures {
            channel_enabled: true,
        },
        PromptFeatures::detect(),
    ];

    let language_combos: [Option<&str>; 3] = [None, Some("zh-CN"), Some("fr")];

    for features in &features_combos {
        for language in &language_combos {
            // No overrides
            {
                let old = build_system_prompt(
                    &MetaHarnessState::default(),
                    no_overrides,
                    cwd,
                    *features,
                    &SkillsProvider,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let collected = crate::session::build_collected_sections(
                    &MetaHarnessState::default(),
                    no_overrides,
                    *language,
                );
                let new = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
                    &env,
                    features,
                    &SkillsProvider,
                    &[],
                );
                assert_eq!(
                    old, new,
                    "byte mismatch: features={:?}, lang={:?}, overrides=None",
                    features, language
                );
            }
            // With non-empty overrides
            {
                let old = build_system_prompt(
                    &MetaHarnessState::default(),
                    Some(&with_overrides),
                    cwd,
                    *features,
                    &SkillsProvider,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let collected = crate::session::build_collected_sections(
                    &MetaHarnessState::default(),
                    Some(&with_overrides),
                    *language,
                );
                let new = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
                    &env,
                    features,
                    &SkillsProvider,
                    &[],
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
                    &MetaHarnessState::default(),
                    Some(&empty_overrides),
                    cwd,
                    *features,
                    &SkillsProvider,
                    &[],
                    Some(frozen_date),
                    *language,
                );
                let env = PromptEnv::with_frozen_date(cwd, frozen_date);
                let collected = crate::session::build_collected_sections(
                    &MetaHarnessState::default(),
                    Some(&empty_overrides),
                    *language,
                );
                let new = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
                    &env,
                    features,
                    &SkillsProvider,
                    &[],
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

/// 验证渲染路径字节一致（C2）：build_system_prompt（经
/// `crate::session::build_collected_sections` 收集）与直接 PromptTemplate + 同一收集结果
/// 输出逐字节一致，并只生成一个 provider 必须消费的 cache boundary token。
#[test]
fn test_template_byte_identical_and_one_boundary_marker() {
    let old = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::detect(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    let env = PromptEnv::detect("/tmp");
    let collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    let new = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &PromptFeatures::detect(),
        &SkillsProvider,
        &[],
    );
    assert_eq!(old, new, "两条渲染路径字节一致");
    assert_eq!(
        new.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(),
        1,
        "生产模板必须携带唯一 cache boundary transport token"
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("Tool usage policy"),
        "full 模式不得移除工具纪律段落（05_using_tools）"
    );
    assert!(
        // v4 引名约定：段落标题也用模型面名字（裸名 `Bash` 已无提供面）
        result.contains("## Bash discipline"),
        "full 模式不得移除 Bash 纪律段落（05_using_tools）"
    );
}

/// full 模式的缓存区前缀必须与 extend 模式完全一致。
///
/// 分层后 full 模式同样渲染不可替换层，缓存区段（01-06，zone=Cached）字节
/// 与非 full 相同，恢复 Anthropic 前缀缓存命中区域的一致性。
#[test]
fn test_render_full_mode_prefix_aligned_with_extend() {
    // 缓存区前缀 = 01-06 段（持有者事实源）按段内序号连接
    let cached_prefix: String = {
        let sections = DefaultSystemPromptMiddleware::sections(None);
        let mut parts = Vec::new();
        for s in sections
            .iter()
            .filter(|s| s.zone == PromptSectionZone::Cached)
        {
            parts.push(s.content.as_str());
        }
        parts.join("\n\n")
    };
    let full_overrides = AgentOverrides {
        persona: Some("You are a custom full-mode agent.".into()),
        tone: None,
        proactiveness: None,
        mode: Some("full".into()),
    };
    let full = build_system_prompt(
        &MetaHarnessState::default(),
        Some(&full_overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    let extend = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        full.starts_with(&cached_prefix),
        "full 模式缓存区前缀 = 01-06 段连接（persona 不进入缓存前缀）"
    );
    assert!(
        extend.starts_with(&cached_prefix),
        "extend 模式缓存区前缀 = 01-06 段连接"
    );
    // 缓存区段（01-06）字节一致 → 前缀缓存命中区域不随 persona 模式变化
    assert_eq!(
        &full[..cached_prefix.len()],
        &extend[..cached_prefix.len()],
        "缓存区前缀字节一致"
    );
}

/// 验证固定层顺序：缓存区段（01-06）→ 07_runtime → gated sections。
#[test]
fn test_render_immutable_layer_order() {
    // frozen_date 参数化，避免触发 chrono::Local::now()（testing-standards 4.1 确定性）
    let features = PromptFeatures {
        channel_enabled: true,
    };
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        features,
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    let safety_pos = result.find("Treat secrets").unwrap(); // 02_system（SafetyAuthorization）
    let engineering_pos = result.find("# Doing tasks").unwrap(); // 03_doing_tasks（EngineeringBehavior）
    let runtime_pos = result.find("<env>").unwrap(); // 07_runtime（RuntimeStateBoundary）
    let gated_pos = result.find("SubAgent Delegation").unwrap(); // 11_subagent（gated）
    assert!(
        safety_pos < engineering_pos,
        "SafetyAuthorization 层应位于 EngineeringBehavior 层之前"
    );
    assert!(
        engineering_pos < runtime_pos,
        "不可替换层（工程行为）应位于运行时段（07_runtime）之前"
    );
    assert!(
        runtime_pos < gated_pos,
        "07_runtime 应位于 gated 段（11_subagent）之前"
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/custom/project",
        PromptFeatures::none(),
        &SkillsProvider,
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
        &MetaHarnessState::default(),
        Some(&overrides_none),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    let result_extend = build_system_prompt(
        &MetaHarnessState::default(),
        Some(&overrides_extend),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
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

/// [迁移守护] 05_using_tools.md 无工具条目残留（design v2 §2.5.5/2.5.6 全量迁移完成态）。
///
/// 全量迁移语义：全部 14 Core + 3 Meta 工具的 `prompt_declaration` 已就位，
/// 05 仅保留通用纪律、Bash discipline 与工具选择原则骨架小节（"Tool selection
/// principles"，不含工具名）——声明段是工具选择指引的单一事实来源（工具代码），
/// 05 不再维护任何工具条目。
#[tokio::test]
async fn test_declaration_segment_is_single_source_and_05_has_no_tool_entries() {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use parking_lot::RwLock;
    use peri_agent::middleware::r#trait::Middleware;
    use peri_agent::tools::BaseTool;
    use peri_middlewares::tool_search::{ToolSearchIndex, ToolSearchMiddleware};
    use peri_middlewares::tools::ReadFileTool;

    let section_05 = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/05_using_tools.md"
    ));
    // 全量迁移完成态：05 无任何工具条目（"Choosing the right tool" 小节已删除）
    assert!(
        !section_05.contains("## Choosing the right tool"),
        "05 不应残留工具条目小节（全量迁移完成）"
    );
    assert!(
        !section_05.contains("**Read a file**"),
        "05 不应残留 Read 手写条目（全量迁移完成）"
    );

    // 经真实装配面收集声明段：ToolSearchMiddleware.before_agent →
    // prompt_contribution()（与 stage_builder 步骤 8 同数据源）
    let mut shared = BTreeMap::new();
    shared.insert(
        "Read".to_string(),
        Arc::new(ReadFileTool::new("/tmp")) as Arc<dyn BaseTool>,
    );
    let mw = ToolSearchMiddleware::new(
        Arc::new(ToolSearchIndex::new()),
        Arc::new(RwLock::new(shared)),
    );
    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    let contribution = Middleware::prompt_contribution(&mw).unwrap();
    assert!(
        contribution.contains("Read a file → `Read` (Read). Use `Read` for file content"),
        "声明段应渲染 Read 模板（title 走 name 派生）：{contribution}"
    );
    // 反向：05 剩余内容不得包含声明段渲染行
    let decl_line = contribution
        .lines()
        .find(|l| l.starts_with("Read a file"))
        .unwrap();
    assert!(
        !section_05.contains(decl_line),
        "05 不得与声明段渲染行逐字重复"
    );
}
