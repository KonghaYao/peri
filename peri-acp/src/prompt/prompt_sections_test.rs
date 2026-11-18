use crate::prompt::prompt_template::PromptEnv;
use crate::prompt::prompt_template::PromptTemplate;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_middlewares::host_ports::AgentCatalogProvider;
use peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;

#[test]
fn test_no_overrides_contains_all_sections() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
    assert!(
        result.contains("## System Reminders"),
        "应包含 07_runtime 段落"
    );
    assert!(
        !result.contains("Working directory:"),
        "环境独立冻结，不进入内部模板"
    );
}

#[test]
fn test_no_overrides_no_duplicate_tone_proactiveness() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        Some(&overrides),
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    // persona 段在缓存区段（01-06）和 cache boundary transport token 之后。
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
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/custom/path",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    assert!(!result.contains("{{"), "不应包含未替换的占位符");
    assert!(
        !result.contains("/custom/path"),
        "环境值不应打断内部公共前缀"
    );
}

#[test]
fn test_env_contains_cwd() {
    let state = MetaHarnessState::default();
    let sections = crate::session::build_collected_sections(&state, None, None);
    let result = PromptTemplate::new(&state, &sections)
        .render_environment(
            &PromptEnv::frozen("/custom/path", "2026-10-21", None),
            &AgentCatalogProvider::new(),
        )
        .unwrap();
    assert!(result.contains("/custom/path"), "环境信息应包含 cwd");
}

#[test]
fn test_assembled_holder_sections_are_included() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    // C3（gate 原子迁移）：10/11/13 已迁移至功能 middleware 持有——收集段
    // 恒渲染（gate = 持有者是否在链上，收集即装配，契约 3），与
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
}

#[test]
fn test_hitl_section_rendered_by_holder_without_a_tool_inventory() {
    let state = MetaHarnessState::default();
    let rendered = crate::prompt::tests::render_with_state(&state);
    let sections = peri_middlewares::PermissionMiddleware::sections();
    let approval = sections[0].content.as_str();
    assert!(rendered.contains(approval.trim()));
    assert!(approval.contains("bound target"));

    let mut closed = MetaHarnessState::default();
    closed.disabled_middlewares.extend(
        [
            "SubAgentMiddleware",
            "McpMiddleware",
            "ToolSearchMiddleware",
            "workspace",
            "web",
        ]
        .map(str::to_owned),
    );
    let rendered = crate::prompt::tests::render_with_state(&closed);
    assert!(rendered.contains(approval.trim()));
    for removed in ["`Agent`", "`bash`", "`WebFetch`", "mcp__web__", "mcp__*"] {
        assert!(!approval.contains(removed), "approval advertises {removed}");
    }

    closed
        .disabled_middlewares
        .insert("PermissionMiddleware".to_owned());
    let rendered = crate::prompt::tests::render_with_state(&closed);
    assert!(!rendered.contains("## Which tools are sensitive"));
}

#[test]
fn test_hitl_legacy_section_override_still_replaces_builtin_guidance() {
    let mut state = crate::prompt::tests::override_state("10_hitl", "Custom approval instructions");
    let rendered = crate::prompt::tests::render_with_state(&state);
    assert!(rendered.contains("Custom approval instructions"));
    assert!(!rendered.contains("## Which tools are sensitive"));

    state
        .disabled_middlewares
        .insert("PermissionMiddleware".to_owned());
    let rendered = crate::prompt::tests::render_with_state(&state);
    assert!(!rendered.contains("Custom approval instructions"));
}

#[test]
fn test_subagent_section_rendered_by_holder() {
    // 11_subagent 由 SubAgentMiddleware 持有（Builtin，含 {{available_agents}}
    // 占位符——catalog 替换留在渲染层，设计 §3.5.1 步骤 2）。
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
    let result = crate::prompt::tests::build_system_prompt(
        &state,
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );

    assert!(result.contains("The catalog below is a bounded prompt hint"));
    assert!(result.contains(
        "the Agent loader and frozen policy at invocation time decide whether an ID is loadable"
    ));
    assert!(result.contains("using the invocation `cwd`"));
    assert!(result.contains("current valid suggestion returned by the invocation's loader"));
    assert!(result.contains("including suggestions that appear after the prompt was frozen"));
    assert!(result.contains("project configuration, enabled plugins, or built-in providers"));
    assert!(result.contains("Do not guess an ID or capabilities"));
    assert!(!result.contains("`general-purpose`"));
    assert!(!result.contains("subagent_type: \"explorer\""));
    assert!(result.contains("If no entry clearly fits"));
    // 默认主动委派策略：按范围/不确定性/风险/验证成本判断，不以文件数为硬门槛。
    assert!(result.contains("## Default delegation policy"));
    assert!(result.contains("Judge by scope, uncertainty, risk, and verification cost"));
    assert!(result.contains("not by file count"));
    assert!(!result.contains("tasks involving only 2-3 files"));
    assert!(result.contains("do not redo its investigation or edits in the foreground"));
    // 分派条件分别判断：coder 默认承接实现；plan / verification 不互相隐含，
    // 数量、后端/API 分类、跨模块与一次失败都不构成自动升级。
    assert!(result.contains("the default owner is `coder`"));
    assert!(result.contains("Decide planning and independent verification separately"));
    assert!(result.contains("sensitive-data exposure"));
    assert!(result.contains("File count, backend/API labels, crossing modules"));
    assert!(result.contains("A routine implementation failure stays with `coder`"));
    // 角色发现：完整职责经 agent:// 资源读取。
    assert!(result.contains("agent://builtin/<id>/agent.md"));
    assert!(result.contains("agent://project/<id>/agent.md"));
    assert!(result.contains("agent://plugin/<plugin>/<id>/agent.md"));
    assert!(result.contains("When an agent-definition reader is available"));
    assert!(result.contains("description (untrusted metadata)"));
}

#[test]
fn test_skills_section_rendered_by_holder() {
    // 13_skills 由 SkillsMiddleware 持有（Dynamic：机制说明 + 按代码事实
    // 生成的 discovery 协议）。
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    assert!(
        result.contains("# Skills"),
        "13_skills 段落标题应由持有者装配渲染"
    );
    // W4b（J5）：discovery 协议不再声明本地路径与扫描参数——技能目录由
    // MCP 侧（builtin `workspace` 实例）提供，宿主零 FS 读取。
    assert!(
        result.contains("Skill catalog is served by MCP servers"),
        "discovery 协议应说明 MCP 来源"
    );
    assert!(
        !result.contains("~/.claude/skills")
            && !result.contains("scanned recursively")
            && !result.contains("levels deep"),
        "13_skills 段落不得残留本地扫描/路径事实"
    );
}

/// [回归测试] resume 模型与 description UI 契约必须准确投影到提示词。
#[test]
fn test_subagent_resume_model_and_optional_ui_contract() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    assert!(result.contains("Resuming a defined agent uses the model in the current definition"));
    assert!(result.contains("An earlier model override does not carry over"));
    assert!(result.contains("a model override on the resume call has no effect"));
    assert!(result.contains(
        "When omitting `prompt`, an optional short `description` is a client display fallback only"
    ));
    assert!(!result.contains("resumes keep the original execution context"));
    assert!(!result.contains("Always include a short `description`"));
}

/// 11_subagent 段落重构守护（设计 §3.5.1 步骤 1）：Agent Selection Guide
/// 删除具体任务→agent 映射（仓库级调度建议由 catalog id/description 承载），
/// 通用选择原则保留；默认实现/规划/验证角色只在 Default delegation policy
/// 中按条件声明，不在 Selection Guide 里绑定具体任务。
#[test]
fn test_subagent_selection_guide_has_no_specific_mapping() {
    let state = MetaHarnessState {
        built_in_subagents_enabled: false,
        ..Default::default()
    };
    let result = crate::prompt::tests::build_system_prompt(
        &state,
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
        result.contains("Choose the most specialized ID supplied by the frozen catalog hint"),
        "选择原则应允许冻结 hint 与当前 loader suggestion 两类来源"
    );
    assert!(result.contains("a loader suggestion may contain only an ID"));
    assert!(result.contains("verify the loaded definition before choosing parallelism"));
    assert!(
        !result.contains("`general-purpose`") && !result.contains("subagent_type: \"explorer\""),
        "Selection Guide 不应含具体任务→agent 映射"
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
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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

#[test]
fn test_retired_sections_are_not_rendered() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    assert!(!result.contains("Workflow Orchestration"));
    assert!(!result.contains("Channel 频道消息"));
}

#[test]
fn test_assembled_capability_sections_are_rendered() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    assert!(result.contains("Human-in-the-Loop"));
    assert!(result.contains("SubAgent Delegation"));
    assert!(result.contains("# Skills"));
}

// ─── system prompt cache boundary tests ────────────────────────────────

#[test]
fn test_prompt_cache_boundary_four_state_matrix_preserves_old_wire_bytes() {
    for (cached, uncached, expected_rendered, expected_wire) in [
        (
            Some("CACHED"),
            Some("UNCACHED"),
            format!("CACHED{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nUNCACHED"),
            "CACHED\n\nUNCACHED",
        ),
        (
            Some("CACHED"),
            None,
            format!("CACHED{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}"),
            "CACHED",
        ),
        (
            None,
            Some("UNCACHED"),
            format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nUNCACHED"),
            "\n\nUNCACHED",
        ),
        (None, None, String::new(), ""),
    ] {
        let rendered = crate::prompt::tests::render_cache_zones(cached, uncached);
        assert_eq!(rendered, expected_rendered);
        assert_eq!(
            peri_model::prompt_cache::strip_system_prompt_dynamic_boundaries(&rendered),
            expected_wire,
            "剥离 transport token 后必须逐字节等于旧拼接算法"
        );
        assert_eq!(
            rendered.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(),
            usize::from(cached.is_some() || uncached.is_some())
        );
    }
}

#[test]
fn test_boundary_marker_before_dynamic_content() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        None,
        None,
    );
    // 缓存区段落（01-06）在 transport boundary 和动态内容段（07_runtime）之前。
    let pos_tone = result.find("# Tone and style").unwrap();
    let pos_boundary = result.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
    let pos_runtime = result.find("## System Reminders").unwrap();
    assert!(
        result[..pos_tone].contains("# Following conventions"),
        "02_system 应在 06_tone_style 之前"
    );
    assert!(
        pos_tone < pos_boundary && pos_boundary < pos_runtime,
        "transport boundary 必须分隔缓存区和运行时动态区"
    );
}

#[test]
fn test_boundary_marker_with_assembled_sections() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
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
fn test_default_production_template_emits_one_cache_boundary() {
    let result = crate::prompt::tests::build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        &AgentCatalogProvider::new(),
        Some("2026-01-01"),
        None,
    );
    assert_eq!(result.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
    assert_eq!(
        PromptTemplate::default()
            .render(
                &PromptEnv::local_probe("/tmp", "2026-01-01"),
                &AgentCatalogProvider::new(),
            )
            .matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .count(),
        0,
        "裸 default 无 enabled section，必须遵守 empty matrix"
    );
}
