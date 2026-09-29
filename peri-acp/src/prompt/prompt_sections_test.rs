use super::*;

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
    // N8：`Bash` 条目原地改名为模型面 effective name（文件系统/Shell 工具
    // 已迁入 `workspace` builtin 实例）。期望值从注册表派生，不落第二份名单。
    let bash_effective = peri_acp_types::builtin_mcp::find("workspace")
        .and_then(|instance| {
            instance
                .tools
                .iter()
                .find(|tool| tool.original_name == "Bash")
        })
        .expect("workspace 实例必须登记 Bash（N8 改名的前提）")
        .effective_name;
    assert!(
        result.contains(&format!("`{bash_effective}` — shell command execution")),
        "sensitive 列表按 default_requires_approval 代码事实生成（条目名为模型面 effective name）"
    );
}

/// S-04（A19 / R33）：10_hitl 渲染出的敏感清单点名 system MCP 原始工具名。
///
/// 清单是动态生成的（`format_sensitive_tools()` 是渲染面唯一事实源）：Web 工具
/// `system_mcp_tools` 选中的 Web 工具模型面使用 `WebFetch` / `WebSearch`。
/// 本用例锁定清单与模型可见名一致，不依赖外部工具同名获得 builtin 规则：
/// ① 渲染结果逐字包含清单（条目表确实进入模型面，同源不漂移）；
/// ② 两个 Web 原名在清单内；
/// ③ `artifact` 不在敏感清单（parity：与原始名 `artifact` 的判定一致）；
/// ④ 条目数 / 前缀条目数不变（14 / 3），与 `permission/mod_test.rs` 的计数断言
///    互为双锁。
#[test]
fn test_hitl_sensitive_list_uses_system_mcp_raw_names() {
    use peri_middlewares::permission::{format_sensitive_tools, sensitive_tool_entries};

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

    let list = format_sensitive_tools();
    assert!(
        result.contains(&list),
        "10_hitl 段落必须逐字包含敏感清单渲染结果（条目表是渲染面单一事实源）"
    );
    assert!(
        list.contains("- `WebFetch` — "),
        "敏感清单应含 system MCP 原名 WebFetch"
    );
    assert!(
        list.contains("- `WebSearch` — "),
        "敏感清单应含 system MCP 原名 WebSearch"
    );
    assert!(
        !list.contains("- `mcp__web__WebFetch` — ") && !list.contains("- `mcp__web__WebSearch` — "),
        "敏感清单不得把历史前缀名作为可调用名展示"
    );
    assert!(
        !list.contains("artifact"),
        "artifact 不在敏感清单（parity：与原始名判定一致）"
    );

    let entries = sensitive_tool_entries();
    assert_eq!(entries.len(), 14, "条目数不变（A19 只改条目名）");
    assert_eq!(
        entries.iter().filter(|e| e.prefix_match).count(),
        3,
        "前缀条目数不变（A19 只改条目名）"
    );
    assert_eq!(
        list.lines().count(),
        entries.len(),
        "渲染清单项数 == 条目表项数（列表与判定同源）"
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
        result.contains("Choose the most specialized ID supplied by the frozen catalog hint"),
        "选择原则应允许冻结 hint 与当前 loader suggestion 两类来源"
    );
    assert!(result.contains("a loader suggestion may contain only an ID"));
    assert!(result.contains("verify the loaded definition before choosing parallelism"));
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
        let rendered = render_cache_zones(cached, uncached);
        assert_eq!(rendered, expected_rendered);
        assert_eq!(
            strip_system_prompt_dynamic_boundaries(&rendered),
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
    // 缓存区段落（01-06）在 transport boundary 和动态内容段（07_runtime）之前。
    let pos_tone = result.find("# Tone and style").unwrap();
    let pos_boundary = result.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
    let pos_runtime = result.find("Working directory").unwrap();
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
fn test_default_production_template_emits_one_cache_boundary() {
    let result = build_system_prompt(
        &MetaHarnessState::default(),
        None,
        "/tmp",
        PromptFeatures::none(),
        &SkillsProvider,
        &[],
        Some("2026-01-01"),
        None,
    );
    assert_eq!(result.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
    assert_eq!(
        PromptTemplate::default()
            .render(
                &PromptEnv::with_frozen_date("/tmp", "2026-01-01"),
                &PromptFeatures::none(),
                &SkillsProvider,
                &[],
            )
            .matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .count(),
        0,
        "裸 default 无 enabled section，必须遵守 empty matrix"
    );
}
