use super::*;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_middlewares::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use peri_middlewares::host_ports::SkillsProvider;
use peri_middlewares::subagent::SubAgentMiddleware;

/// 构建系统提示词（测试 helper；原 prompt/mod.rs 的同名函数随 §0 边 2
/// 依赖门收口迁出——收集函数移至宿主装配面 `crate::session::crate::session::build_collected_sections`，
/// 本 helper 仅测试直接调用，收于测试模块）。
///
/// 从持有者段落 + `prompts/sections/` 目录按固定顺序加载段落：基础段落
/// （01-06 / 07_runtime / persona / language）与 gated 段（10_hitl /
/// 11_subagent / 13_skills，经 [`crate::session::crate::session::build_collected_sections`]
/// 收集）始终包含（除非持有 middleware 被关闭）；15_channel 按
/// `PromptFeatures` 条件注入（gate 恒 false）；环境占位符替换为运行时值。
///
/// `overrides` 存在时，将 agent.md 中定义的角色/风格/主动性拼成一个
/// Persona 段（`DefaultSystemPromptMiddleware` 动态生成）；`prompt_mode:
/// full` 时 body 仅替换 Persona 段，基础段落仍保留；为 `None` 时覆盖块
/// 为空（Persona 段不渲染）。
#[allow(clippy::too_many_arguments)] // 渲染面固定参数集（与生产构造点一致）
fn build_system_prompt(
    meta_harness: &MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    skills: &dyn SkillsPort,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String {
    let collected = crate::session::build_collected_sections(meta_harness, overrides, language);
    let template = PromptTemplate::new(meta_harness, &collected);
    let env = if let Some(date) = frozen_date {
        PromptEnv::with_frozen_date(cwd, date)
    } else {
        PromptEnv::detect(cwd)
    };
    template.render(&env, &features, skills, extra_agent_dirs)
}

#[test]
fn test_no_overrides_contains_all_sections() {
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
        result.contains("Following conventions"),
        "应包含 02_system 段落"
    );
    assert!(result.contains("Doing tasks"), "应包含 03_doing_tasks 段落");
    assert!(
        result.contains("Ask Before Diving"),
        "应包含 03_doing_tasks Ask Before Diving 段落"
    );
    assert!(
        result.contains("Batch independent tool calls"),
        "应包含 05_using_tools 通用工具纪律（工具条目已迁移至声明段）"
    );
    assert!(result.contains("<env>"), "应包含 07_runtime 段落");
    assert!(
        result.contains("Working directory"),
        "应包含 08_env 替换后结果"
    );
}

#[test]
fn test_no_overrides_no_duplicate_tone_proactiveness() {
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
    // "# Tone and style" 仅出现 1 次（来自 06_tone_style.md 静态段落，不来自覆盖块）
    assert_eq!(
        result.matches("# Tone and style").count(),
        1,
        "无 overrides 时 # Tone and style 应仅出现 1 次（来自静态段落）"
    );
    // "# Proactiveness" 仅出现 1 次（来自 03_doing_tasks.md 静态段落，
    // C2：02_system 的 Proactiveness 块已并入 03）
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
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    // persona 段在缓存区段（01-06）之后（C2：boundary 文本标记已删除，
    // 位置属性承担划分）
    let pos_tone_style = result.find("# Tone and style").unwrap();
    assert!(
        result[pos_tone_style..].contains("test persona"),
        "有 overrides 时缓存区段之后应包含 persona 内容"
    );
    // 静态段（01-06）不应包含 persona 内容
    assert!(
        !result[..pos_tone_style].contains("test persona"),
        "persona 不应在缓存段内"
    );
}

#[test]
fn test_placeholders_replaced() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/custom/path",
        PromptFeatures::none(),
        &SkillsProvider,
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
        &MetaHarnessState::default(),
        None,
        "/custom/path",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    assert!(result.contains("/custom/path"), "环境信息应包含 cwd");
}

#[test]
fn test_features_none_excludes_only_unheld_channel_section() {
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
    // C3（gate 原子迁移）：10/11/13 已迁移至功能 middleware 持有——收集段
    // 恒渲染（gate = 持有者是否在链上，收集即装配，契约 3），与
    // PromptFeatures 字段无关。
    assert!(
        result.contains("Human-in-the-Loop"),
        "10_hitl 收集段恒渲染（持有者装配即渲染）"
    );
    assert!(
        result.contains("SubAgent Delegation"),
        "11_subagent 收集段恒渲染"
    );
    assert!(
        result.contains("# Skills"),
        "13_skills 收集段恒渲染（标题保留）"
    );
    // 15_channel 无持有者：gate 恒 false（PromptFeatures::none），不渲染
    assert!(
        !result.contains("Channel 频道消息"),
        "15_channel 无持有者，按 FeatureGate::Channel 门控"
    );
}

#[test]
fn test_hitl_section_rendered_by_holder() {
    // 10_hitl 由 PermissionMiddleware 持有（2026-08-15 拆分：Dynamic：机制
    // 说明 + 按代码事实生成的 sensitive 列表）；收集段恒渲染，不依赖
    // hitl_enabled 字段。
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
        result.contains("Human-in-the-Loop"),
        "10_hitl 段落应由持有者装配渲染"
    );
    assert!(
        result.contains("`Bash` — shell command execution"),
        "sensitive 列表按 default_requires_approval 代码事实生成"
    );
}

#[test]
fn test_subagent_section_rendered_by_holder() {
    // 11_subagent 由 SubAgentMiddleware 持有（Builtin，含 {{available_agents}}
    // 占位符——catalog 替换留在渲染层，设计 §3.5.1 步骤 2）。
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
        result.contains("SubAgent Delegation"),
        "11_subagent 段落应由持有者装配渲染"
    );
}

#[test]
fn test_subagent_section_does_not_hardcode_built_in_agent_ids() {
    let state = MetaHarnessState {
        built_in_subagents_enabled: false,
        ..Default::default()
    };
    let result = build_system_prompt(
        &state,
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );

    assert!(result.contains("The catalog below is the authoritative list"));
    assert!(result.contains("project configuration, enabled plugins, or built-in providers"));
    assert!(result.contains("Do not guess capabilities"));
    assert!(!result.contains("`general-purpose`"));
    assert!(!result.contains("subagent_type: \"explorer\""));
    assert!(result.contains("If no entry clearly fits"));
}

#[test]
fn test_skills_section_rendered_by_holder() {
    // 13_skills 由 SkillsMiddleware 持有（Dynamic：机制说明 + 按代码事实
    // 生成的 discovery 协议）。
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
        result.contains("# Skills"),
        "13_skills 段落标题应由持有者装配渲染"
    );
    // discovery 协议按代码事实生成（loader 常量格式化注入，防手写漂移）
    assert!(
        result.contains(
            "Each skill root is scanned recursively up to 6 levels deep (max 1000 directories per root)"
        ),
        "discovery 扫描参数应来自 loader 常量（MAX_SCAN_DEPTH / MAX_SKILLS_DIRS_PER_ROOT）"
    );
    assert!(
        result.contains("1. `~/.claude/skills/` — user-level skills (highest priority)"),
        "discovery roots 优先级应动态生成（User 最高）"
    );
}

/// 11_subagent 段落重构守护（设计 §3.5.1 步骤 1）：Agent Selection Guide
/// 删除具体任务→agent 映射（仓库级调度建议由 catalog id/description 承载），
/// 通用选择原则保留，但不绑定任何 built-in agent ID。
#[test]
fn test_subagent_selection_guide_has_no_specific_mapping() {
    let state = MetaHarnessState {
        built_in_subagents_enabled: false,
        ..Default::default()
    };
    let result = build_system_prompt(
        &state,
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        None,
        None,
    );
    // 具体映射与 pipelines 已删除
    assert!(
        !result.contains("Code implementation / editing / refactoring / migration"),
        "Selection Guide 不应含具体任务→agent 映射"
    );
    assert!(
        !result.contains("**Standard pipelines**"),
        "Standard pipelines 具体建议已删除"
    );
    // 通用选择原则保留（不绑定 agent 名）
    assert!(
        result.contains("Choose the most specialized agent whose catalog ID"),
        "选择原则应以当前 catalog 为事实源并优先 specialized agent"
    );
    assert!(
        !result.contains("`general-purpose`") && !result.contains("subagent_type: \"explorer\""),
        "静态段落不应硬编码 built-in agent ID"
    );
    assert!(
        result
            .contains("`readonly` agents may run concurrently, `writes` agents must be sequenced"),
        "按 access 标签并行化的通用原则应保留"
    );
}

/// 段落位置顺序守护（契约 2）：非缓存区按段内序号——10_hitl(3) →
/// 11_subagent(4) → 13_skills(5) → language(7)，不依赖 middleware 链序。
#[test]
fn test_gated_sections_render_in_position_order() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        Some("zh"),
    );
    let pos_hitl = result.find("Human-in-the-Loop").unwrap();
    let pos_subagent = result.find("SubAgent Delegation").unwrap();
    let pos_skills = result.find("# Skills").unwrap();
    let pos_lang = result.find("# Language").unwrap();
    assert!(
        pos_hitl < pos_subagent && pos_subagent < pos_skills && pos_skills < pos_lang,
        "gated 段按段内序号渲染：{pos_hitl} < {pos_subagent} < {pos_skills} < {pos_lang}"
    );
}

/// [回归测试] 16_workflow 已整段删除（波 4 演进 C2，ultracode skill 完整
/// 覆盖——设计 §3.1.2）：渲染输出与 gate 结构均不得再出现 workflow 段落。
#[test]
fn test_workflow_section_deleted_entirely() {
    // GATED_SECTIONS 不再包含 16_workflow（无持有者 gate 清理）
    assert!(
        !GATED_SECTIONS
            .iter()
            .any(|(id, _, _, _)| id.contains("16_workflow")),
        "16_workflow 不应再位于 GATED_SECTIONS"
    );
    // 渲染输出不含 workflow 声明（默认配置 + channel 开启均不含）
    let features_channel = PromptFeatures {
        channel_enabled: true,
    };
    for features in [PromptFeatures::none(), features_channel] {
        let result = build_system_prompt(
            &MetaHarnessState::default(),
            None,
            "/tmp",
            features,
            &SkillsProvider,
            &[],
            None,
            None,
        );
        assert!(
            !result.contains("Workflow Orchestration"),
            "16_workflow 段落已删除：任何 gate 组合都不应渲染"
        );
    }
}

#[test]
fn test_all_features_enabled_includes_all() {
    // 10/11/13 由持有者装配渲染（收集段恒渲染）；channel_enabled=true 时
    // 15_channel 渲染（FeatureGate::Channel 判定，无持有者）。
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
        None,
        None,
    );
    assert!(result.contains("Human-in-the-Loop"), "应包含 HITL 段落");
    assert!(
        result.contains("SubAgent Delegation"),
        "应包含 SubAgent 段落"
    );
    assert!(result.contains("# Skills"), "应包含 Skills 段落标题");
    assert!(result.contains("Channel 频道消息"), "应包含 Channel 段落");
}

#[test]
fn test_detect_default_values() {
    let features = PromptFeatures::detect();
    // C3：hitl/subagent/skills gate 已随段落实体迁移（收集即装配，契约 3），
    // detect 仅剩 channel gate（15_channel 无持有者，恒 false）。
    assert!(
        !features.channel_enabled,
        "detect() 不得把未装配的 channel 宣称为可用能力"
    );
}

/// [回归测试] 未装配的 channel 不作为运行时能力（P3-2026-08-02）。
///
/// 16_workflow 已删除（C2），无子面向 feature 差异；15_channel 恒不渲染。
#[test]
fn test_detect_channel_gate_never_enabled() {
    let features = PromptFeatures::detect();
    assert!(
        !features.channel_enabled,
        "未装配 ChannelOwner 时 channel 恒不启用"
    );
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        features,
        &SkillsProvider,
        &[],
        None,
        None,
    );
    assert!(
        !result.contains("Channel 频道消息"),
        "未装配 channel 时 prompt 不得包含 15_channel"
    );
}

// ─── boundary marker tests ──────────────────────────────────────────────

#[test]
fn test_boundary_marker_removed() {
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
        !result.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"),
        "波 4 演进（C2）：boundary 文本标记已删除（缓存区划分由段落位置属性承担）"
    );
}

#[test]
fn test_boundary_marker_before_dynamic_content() {
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
    // 缓存区段落（01-06）在动态内容段（07_runtime）之前——位置属性
    // （zone + 段内序号）承担缓存区划分（C2：boundary 文本标记已删除）
    let pos_tone = result.find("# Tone and style").unwrap();
    assert!(
        result[..pos_tone].contains("# Following conventions"),
        "02_system 应在 06_tone_style 之前"
    );
    assert!(
        result[pos_tone..].contains("Working directory"),
        "07_runtime 应在 06_tone_style 之后（缓存区后段）"
    );
}

#[test]
fn test_boundary_marker_with_all_features() {
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
        None,
        None,
    );
    // feature-gated 段落都在缓存区段（06_tone_style）之后
    let pos_tone = result.find("# Tone and style").unwrap();
    assert!(
        result[pos_tone..].contains("Human-in-the-Loop"),
        "HITL 段落应在缓存区段之后"
    );
    assert!(
        result[pos_tone..].contains("SubAgent Delegation"),
        "SubAgent 段落应在缓存区段之后"
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
    // 之前（现状顺序保持；C2：boundary 文本标记删除，位置属性承担划分）
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
    // Language 段在非缓存区最后（07_runtime 之后；C2：boundary 文本标记
    // 已删除，位置属性承担划分——language 由 LangMiddleware 持有）
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
/// 输出逐字节一致；boundary 文本标记已删除（C2 断言）。
#[test]
fn test_template_byte_identical_and_no_boundary_marker() {
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
    assert!(
        !new.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"),
        "boundary 文本标记已删除（位置属性承担缓存区划分）"
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

// ─── MetaHarness 段落覆盖（设计 §2.4）───────────────────────────────────────

use std::sync::Arc;

/// 构造只含一个段落覆盖的 MetaHarnessState
fn override_state(id: &str, content: &str) -> MetaHarnessState {
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert(id.to_string(), Arc::from(content.to_string()));
    state
}

fn render_with_state(state: &MetaHarnessState, features: PromptFeatures) -> String {
    build_system_prompt(
        state,
        None,
        "/tmp",
        features,
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    )
}

/// 持有者侧段落内容（C2 起基础段由 `DefaultSystemPromptMiddleware` 持有，
/// 测试以持有者声明为事实源，替代已删除的内置数组）。
fn holder_section_content(id: &str) -> String {
    let sections = DefaultSystemPromptMiddleware::sections(None);
    sections
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("段落 {id} 应由 DefaultSystemPromptMiddleware 持有"))
        .content
        .as_str()
        .to_string()
}

#[test]
fn meta_harness_override_replaces_section_full_text() {
    let state = override_state("01_intro", "# Custom Intro\n\n完全替换的角色定义。");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(result.contains("# Custom Intro"), "覆盖内容应出现在输出中");
    // 内置 01_intro 不再出现：以持有者段落全文为锚点校验
    let builtin = holder_section_content("01_intro");
    assert!(
        !result.contains(&builtin),
        "内置 01_intro 全文不应再出现在输出中"
    );
}

#[test]
fn meta_harness_override_05_using_tools() {
    let state = override_state("05_using_tools", "### Tools Discipline\n\n自定义工具纪律。");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(result.contains("自定义工具纪律"), "覆盖内容应出现");
    let builtin = holder_section_content("05_using_tools");
    assert!(!result.contains(&builtin), "内置 05_using_tools 不应再出现");
}

#[test]
fn meta_harness_unoverridden_section_unchanged() {
    let state = override_state("01_intro", "custom");
    let result = render_with_state(&state, PromptFeatures::none());
    let builtin_02 = holder_section_content("02_system");
    assert!(result.contains(&builtin_02), "未覆盖的 02_system 字节不变");
}

#[test]
fn meta_harness_multiple_overrides_apply_together() {
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert("01_intro".to_string(), Arc::from("intro-ovr"));
    state
        .section_overrides
        .insert("05_using_tools".to_string(), Arc::from("tools-ovr"));
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(result.contains("intro-ovr"));
    assert!(result.contains("tools-ovr"));
    // 段落顺序不变：01 在 05 之前
    assert!(
        result.find("intro-ovr").unwrap() < result.find("tools-ovr").unwrap(),
        "段落渲染顺序不变（01_intro 在 05_using_tools 之前）"
    );
}

#[test]
fn meta_harness_override_not_trimmed() {
    // override 内容保留原样（不 trim）：前后空白原样进入输出
    let state = override_state("01_intro", "\n  # Padded  \n\nbody  \n");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(
        result.contains("\n  # Padded  \n\nbody  \n"),
        "覆盖内容不被 trim"
    );
}

#[test]
fn meta_harness_override_placeholders_still_substituted() {
    // override 内容中的占位符参与渲染期替换（与内置段落同一通道）
    let state = override_state("01_intro", "cwd={{cwd}} platform={{platform}}");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(result.contains("cwd=/tmp"), "{{cwd}} 被替换");
    assert!(result.contains("platform="), "{{platform}} 被替换");
}

#[test]
fn meta_harness_gated_enabled_shows_override() {
    // 10_hitl 由 PermissionMiddleware 持有（收集即装配）：覆盖 10_hitl
    // 应生效（覆盖 = 替换持有者对应段落贡献，设计 §2.4 / §3.5.1 步骤 5）。
    let state = override_state("10_hitl", "HITL-OVERRIDE");
    let features = PromptFeatures::detect();
    let result = render_with_state(&state, features);
    assert!(result.contains("HITL-OVERRIDE"), "持有者装配时显示覆盖内容");
}

/// 11_subagent 覆盖语义（设计 §3.5.1 步骤 5 / C3 D2 第 6 步）：覆盖 =
/// 替换 SubAgentMiddleware 持有段落贡献，机制与持有者无关——覆盖全文
/// 出现、内置全文消失、段落渲染位置不变（仍按段内序号在 10_hitl 与
/// 13_skills 之间）。
#[test]
fn meta_harness_override_11_subagent_replaces_holder_section() {
    let state = override_state("11_subagent", "SUBAGENT-OVERRIDE");
    let result = render_with_state(&state, PromptFeatures::detect());
    assert!(
        result.contains("SUBAGENT-OVERRIDE"),
        "覆盖全文应替换持有者段落"
    );
    // 内置 11_subagent（含占位符的 Builtin 文本）全文不再出现
    let builtin = SubAgentMiddleware::sections()[0]
        .content
        .as_str()
        .to_string();
    assert!(
        !result.contains(&builtin),
        "内置 11_subagent 全文不应再出现在输出中"
    );
    // 段落顺序不变：11_subagent 位置仍在 10_hitl（order=3）与
    // 13_skills（order=5）之间
    let pos_hitl = result
        .find("## Which tools are sensitive")
        .expect("10_hitl 机制说明应在");
    let pos_override = result
        .find("SUBAGENT-OVERRIDE")
        .expect("11_subagent 覆盖段应在");
    let pos_skills = result.find("# Skills").expect("13_skills 机制说明应在");
    assert!(
        pos_hitl < pos_override && pos_override < pos_skills,
        "段落顺序不变（10_hitl < 11_subagent < 13_skills）：{pos_hitl} < {pos_override} < {pos_skills}"
    );
}

/// C3（gate 原子迁移）：10_hitl gate = PermissionMiddleware 是否在链上，
/// 不再依赖 permission_mode——`detect()` 无 gate 差异，覆盖恒渲染；关闭
/// 持有者（disabled_middlewares）才隐藏段落（决策记录 C3 D5；2026-08-15
/// 职责拆分：10_hitl 持有者由新 HumanInTheLoopMiddleware 改为
/// PermissionMiddleware）。
#[test]
fn meta_harness_gated_override_hidden_only_when_holder_disabled() {
    let mut state = override_state("10_hitl", "HITL-OVERRIDE");
    let features = PromptFeatures::detect();
    // 默认状态：持有者装配（收集段恒渲染）→ 覆盖显示
    let result = render_with_state(&state, features);
    assert!(
        result.contains("HITL-OVERRIDE"),
        "持有者装配时覆盖应显示（gate 不再依赖 permission_mode）"
    );
    // 关闭持有者 → 段落整体消失（覆盖随段落一并隐藏）
    state
        .disabled_middlewares
        .insert("PermissionMiddleware".to_string());
    let result = render_with_state(&state, features);
    assert!(
        !result.contains("HITL-OVERRIDE"),
        "关闭 PermissionMiddleware 后 10_hitl（含覆盖）不渲染"
    );
}

#[test]
fn meta_harness_persona_full_keeps_overridden_immutable_sections() {
    let state = override_state("01_intro", "OVR-INTRO");
    let overrides = AgentOverrides {
        persona: Some("You are the full persona".into()),
        tone: Some("ignored".into()),
        proactiveness: Some("ignored".into()),
        mode: Some("full".into()),
    };
    let result = build_system_prompt(
        &state,
        Some(&overrides),
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    assert!(
        result.contains("OVR-INTRO"),
        "full persona 不移除覆盖后的 immutable sections"
    );
    assert!(
        result.contains("You are the full persona"),
        "full body 渲染"
    );
}

#[test]
fn meta_harness_build_and_template_byte_identical() {
    // 同源一致性：build_system_prompt 与直接 PromptTemplate render 字节一致
    let state = override_state("01_intro", "OVR-INTRO");
    let overrides = AgentOverrides {
        persona: Some("extend persona".into()),
        tone: None,
        proactiveness: None,
        mode: None,
    };
    let features = PromptFeatures::detect();
    let via_build = build_system_prompt(
        &state,
        Some(&overrides),
        "/tmp",
        features,
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        Some("zh"),
    );
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let collected = crate::session::build_collected_sections(&state, Some(&overrides), Some("zh"));
    let via_template =
        PromptTemplate::new(&state, &collected).render(&env, &features, &SkillsProvider, &[]);
    assert_eq!(via_build, via_template, "两条渲染路径字节一致");
}

#[test]
fn meta_harness_disabled_set_does_not_affect_sections() {
    // 两动作独立：disabled_middlewares 不影响段落渲染
    let mut state = MetaHarnessState::default();
    state
        .section_overrides
        .insert("01_intro".to_string(), Arc::from("OVR-INTRO"));
    state
        .disabled_middlewares
        .insert("WebMiddleware".to_string());
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(result.contains("OVR-INTRO"), "disabled 集合不影响覆盖渲染");
}

#[test]
fn section_ids_match_arrays_and_holders() {
    use peri_acp_types::meta_harness::SECTION_IDS;

    // C3：基础段（01-06 / 07_runtime / persona / language）由
    // DefaultSystemPromptMiddleware / LangMiddleware 持有，gated 段
    // （10_hitl / 11_subagent / 13_skills）由功能 middleware 持有，
    // 15_channel 由 GATED_SECTIONS 数组持有——并集必须与 SECTION_IDS
    // 完全一致（无重复）。
    let mut actual: Vec<&str> = GATED_SECTIONS
        .iter()
        .map(|(id, _, _, _)| *id)
        .chain(
            DefaultSystemPromptMiddleware::sections(None)
                .iter()
                .map(|s| s.id),
        )
        .chain(LangMiddleware::sections(Some("zh")).iter().map(|s| s.id))
        .chain(
            peri_middlewares::permission::PermissionMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::hitl::HumanInTheLoopMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::subagent::SubAgentMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .chain(
            peri_middlewares::skills::SkillsMiddleware::sections()
                .iter()
                .map(|s| s.id),
        )
        .collect();
    actual.sort_unstable();
    let mut expected: Vec<&str> = SECTION_IDS.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected, "SECTION_IDS 与持有者声明 ID 完全一致");
    // 无重复
    let mut seen = std::collections::HashSet::new();
    for id in actual {
        assert!(seen.insert(id), "duplicate section id: {id}");
    }
}

#[test]
fn meta_harness_override_13_skills_gated() {
    // C3：13_skills 由 SkillsMiddleware 持有（收集即装配）：覆盖 13_skills
    // 应生效（持有者装配即渲染）
    let state = override_state("13_skills", "SKILLS-OVERRIDE");
    let features = PromptFeatures::detect();
    let result = render_with_state(&state, features);
    assert!(result.contains("SKILLS-OVERRIDE"));
}

/// P2-2（实施质量审查）：persona 段恒声明 + 空内容——无 overrides 时收集的
/// persona 段内容为空串（渲染面空内容过滤跳过，默认不渲染），但 MetaHarness
/// 覆盖 `.peri/meta/persona.md` 仍可注入（覆盖合并先于空内容过滤）。
#[test]
fn meta_harness_override_persona_without_overrides() {
    // 1. 无 overrides：persona 段恒声明且内容为空（空内容默认不渲染的前提）
    let collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    let persona = collected
        .iter()
        .find(|s| s.id == "persona")
        .expect("persona 段应恒声明（D2）");
    assert!(
        persona.content.as_str().is_empty(),
        "无 overrides 时 persona 内容应为空串"
    );
    // 2. 无用户配置时覆盖仍可注入：覆盖全文渲染（唯一标记）
    let state = override_state("persona", "PERSONA-OVERRIDE-NO-CONFIG");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(
        result.contains("PERSONA-OVERRIDE-NO-CONFIG"),
        "无 overrides 时 persona 覆盖仍应注入"
    );
    // 3. 默认（无覆盖）不渲染空 persona：覆盖标记不出现，段落位置无空残留
    let default_result = render_with_state(&MetaHarnessState::default(), PromptFeatures::none());
    assert!(
        !default_result.contains("PERSONA-OVERRIDE-NO-CONFIG"),
        "无覆盖时默认输出不含 persona 覆盖标记"
    );
}

/// P2-2（实施质量审查）：language 段恒声明 + 空内容——无 `settings.language`
/// 时收集的 language 段内容为空串（默认不渲染），但 MetaHarness 覆盖
/// `.peri/meta/language.md` 仍可注入。
#[test]
fn meta_harness_override_language_without_config() {
    // 1. 无语言配置：language 段恒声明且内容为空（空内容默认不渲染的前提）
    let collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    let language = collected
        .iter()
        .find(|s| s.id == "language")
        .expect("language 段应恒声明（D2）");
    assert!(
        language.content.as_str().is_empty(),
        "无语言配置时 language 内容应为空串"
    );
    // 2. 无语言配置时覆盖仍可注入：覆盖全文渲染（唯一标记）
    let state = override_state("language", "LANGUAGE-OVERRIDE-NO-CONFIG");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(
        result.contains("LANGUAGE-OVERRIDE-NO-CONFIG"),
        "无语言配置时 language 覆盖仍应注入"
    );
    // 3. 默认（无覆盖）不渲染空 language 段：覆盖标记不出现
    let default_result = render_with_state(&MetaHarnessState::default(), PromptFeatures::none());
    assert!(
        !default_result.contains("LANGUAGE-OVERRIDE-NO-CONFIG"),
        "无覆盖时默认输出不含 language 覆盖标记"
    );
}

// ─── 波 4 段落持有者基础设施（C1）：装配期收集结果合并 ──────────────────

fn collected_section(
    id: &'static str,
    zone: PromptSectionZone,
    order: u16,
    content: &'static str,
) -> PromptSection {
    PromptSection::builtin(id, zone, order, content)
}

/// 契约 2：收集段落按"位置 + 段内序号"排序渲染，**不依赖链序**。
///
/// collected 以乱序传入（zz order=9 在前、aa order=8 在后），渲染必须按
/// 段内序号升序输出；非缓存区段落在 07_runtime 之后、Language 段之前
/// （language order=7 < aa order=8 < zz order=9）。
#[test]
fn collected_sections_render_in_position_order() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, Some("zh-CN"));
    // 乱序追加收集段（收集不承诺顺序，排序由渲染面执行）
    collected.push(collected_section(
        "zz_collected",
        PromptSectionZone::Uncached,
        9,
        "ZZ-COLLECTED-LATE",
    ));
    collected.push(collected_section(
        "aa_collected",
        PromptSectionZone::Uncached,
        8,
        "AA-COLLECTED-EARLY",
    ));
    let features = PromptFeatures::detect();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    let pos_runtime = result.find("## System Reminders").unwrap();
    let pos_lang = result.find("# Language").unwrap();
    let pos_aa = result.find("AA-COLLECTED-EARLY").unwrap();
    let pos_zz = result.find("ZZ-COLLECTED-LATE").unwrap();
    assert!(
        pos_runtime < pos_lang && pos_lang < pos_aa && pos_aa < pos_zz,
        "收集段落按段内序号升序渲染（不依赖收集顺序）：{pos_runtime} < {pos_lang} < {pos_aa} < {pos_zz}"
    );
    // 基础段由收集注入（C2：数组已删除，收集结果成为唯一来源）
    assert!(
        result.contains("Following conventions"),
        "02_system 经收集结果渲染"
    );
    assert!(
        result.contains("## System Reminders"),
        "07_runtime 经收集渲染"
    );
}

/// 契约 2 + 收集合并：collected 按 ID 覆盖内置段落，位置属性以持有者声明为准。
#[test]
fn collected_section_overrides_builtin_by_id() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    // 01_intro 由收集段按 ID 覆盖（位置属性 Cached + order 1 与持有者一致）
    collected.retain(|s| s.id != "01_intro");
    collected.push(collected_section(
        "01_intro",
        PromptSectionZone::Cached,
        1,
        "COLLECTED-INTRO",
    ));
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    assert!(result.contains("COLLECTED-INTRO"), "收集段落内容渲染");
    assert!(
        !result.contains("Assist with defensive security tasks"),
        "内置 01_intro 被收集段按 ID 替换"
    );
    // 位置：缓存区首位（02_system 之前）——持有者声明的 Cached+1
    let pos_intro = result.find("COLLECTED-INTRO").unwrap();
    let pos_system = result.find("Following conventions").unwrap();
    assert!(
        pos_intro < pos_system,
        "收集段位置属性（Cached order=1）生效"
    );
}

/// 契约 4：middleware 提供空内容段落 = 跳过渲染不 fail，其余段落不受影响。
#[test]
fn collected_empty_content_skipped() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_empty",
        PromptSectionZone::Uncached,
        8,
        "",
    ));
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    assert!(!result.contains("zz_empty"), "空内容段落不渲染");
    assert!(result.contains("Following conventions"), "其他段落不受影响");
}

/// 动态内容段落（`PromptSectionContent::Dynamic`）正常渲染。
#[test]
fn collected_dynamic_content_rendered() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(PromptSection::dynamic(
        "zz_dyn",
        PromptSectionZone::Uncached,
        8,
        "DYNAMIC-COLLECTED".to_string(),
    ));
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    assert!(result.contains("DYNAMIC-COLLECTED"), "动态内容段落渲染");
}

/// 覆盖语义（设计 §2.4/3.5.1 步骤 5）：MetaHarness 覆盖 = 替换持有者对应段落
/// 贡献——`state.section_overrides` 优先于 collected 内容。
#[test]
fn collected_content_merged_with_meta_harness_override() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.retain(|s| s.id != "05_using_tools");
    collected.push(collected_section(
        "05_using_tools",
        PromptSectionZone::Cached,
        5,
        "COLLECTED-TOOLS",
    ));
    let state = override_state("05_using_tools", "OVERRIDE-TOOLS");
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result =
        PromptTemplate::new(&state, &collected).render(&env, &features, &SkillsProvider, &[]);
    assert!(result.contains("OVERRIDE-TOOLS"), "覆盖全文替换持有者段落");
    assert!(
        !result.contains("COLLECTED-TOOLS"),
        "覆盖优先于 collected 内容"
    );
}

/// 收集段不受 gate 硬编码影响（gate = 持有者是否在链上，收集即装配）：
/// `PromptFeatures::none()` 下收集段仍渲染。
#[test]
fn collected_sections_render_regardless_of_feature_gates() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_collected",
        PromptSectionZone::Uncached,
        8,
        "GATE-FREE-COLLECTED",
    ));
    let features = PromptFeatures::none(); // 全部 gate 关闭
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    assert!(result.contains("GATE-FREE-COLLECTED"), "收集段恒渲染");
    // 对照：未迁移内置 gated 段落（15_channel）仍按硬编码 gate 关闭
    assert!(
        !result.contains("Channel 频道消息"),
        "内置 gated 段（15_channel）不受收集影响，按 PromptFeatures 门控"
    );
}

// ─── C 扩展 / E 补测试（2026-08-14 advisor 矩阵缺口）───────────────────────

/// 覆盖语义边界（empty 定义，C 项裁定）：空串覆盖 → `is_empty()` 过滤 →
/// 段落整体消失（与契约 4"未提供内容 = 跳过渲染"同一路径）。
#[test]
fn meta_harness_override_empty_removes_section() {
    let state = override_state("01_intro", "");
    let result = render_with_state(&state, PromptFeatures::none());
    assert!(
        !result.contains("Assist with defensive security tasks"),
        "空串覆盖 → 01_intro 经 is_empty 过滤从输出消失"
    );
    assert!(result.contains("Following conventions"), "其余段落不受影响");
}

/// 覆盖语义边界（empty 定义锁定）：空白串覆盖 → `is_empty()` 为 false →
/// 原样渲染，不 trim 也不消失（与 `meta_harness_override_not_trimmed`
/// 既定不 trim 语义一致；空白段落保留原位）。
#[test]
fn meta_harness_override_whitespace_renders_as_is() {
    let state = override_state("01_intro", "   ");
    let result = render_with_state(&state, PromptFeatures::none());
    // 01_intro 为缓存区首段，渲染结果以其内容开头（无前缀分隔符）
    assert!(
        result.starts_with("   "),
        "空白覆盖原样渲染（不 trim）：{:?}",
        &result[..result.len().min(24)]
    );
    assert!(
        result.contains("Following conventions"),
        "空白覆盖不触发空过滤，段落保留"
    );
}

/// 收集契约（C 项矩阵）：collected 中重复 ID → 后者覆盖前者（位置属性随
/// 后者声明），渲染恰好一次。
#[test]
fn collected_duplicate_id_last_wins() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_dup",
        PromptSectionZone::Uncached,
        8,
        "DUP-FIRST",
    ));
    collected.push(collected_section(
        "zz_dup",
        PromptSectionZone::Uncached,
        9,
        "DUP-SECOND",
    ));
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    assert!(
        result.contains("DUP-SECOND") && !result.contains("DUP-FIRST"),
        "重复 ID 后者覆盖前者"
    );
    assert_eq!(
        result.matches("DUP-SECOND").count(),
        1,
        "重复 ID 段渲染恰好一次"
    );
}

/// 收集契约（C 项矩阵）：同 (zone, order) 的收集段 → stable 排序保持
/// 收集声明顺序（`sort_by_key` 稳定，不依赖链序的兜底语义）。
#[test]
fn collected_same_zone_order_stable() {
    let mut collected =
        crate::session::build_collected_sections(&MetaHarnessState::default(), None, None);
    collected.push(collected_section(
        "zz_s1",
        PromptSectionZone::Uncached,
        8,
        "STABLE-FIRST",
    ));
    collected.push(collected_section(
        "zz_s2",
        PromptSectionZone::Uncached,
        8,
        "STABLE-SECOND",
    ));
    let features = PromptFeatures::none();
    let env = PromptEnv::with_frozen_date("/tmp", "2026-01-01");
    let result = PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &env,
        &features,
        &SkillsProvider,
        &[],
    );
    let pos_first = result.find("STABLE-FIRST").unwrap();
    let pos_second = result.find("STABLE-SECOND").unwrap();
    assert!(
        pos_first < pos_second,
        "同 (zone, order) 稳定排序保持声明顺序：{pos_first} < {pos_second}"
    );
}
