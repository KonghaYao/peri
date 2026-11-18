//! 冻结期 prompt、skills 与 MetaHarness 渲染装配。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::agents::AgentOverrides;

use super::SessionManager;

impl SessionManager {
    /// 构建会话级 frozen 数据（统一构造入口，消除 TUI/stdio 重复 5 处）。
    ///
    /// 波 4 演进（C2/C5）：16_workflow 已整段删除（ultracode skill 完整覆盖，
    /// 设计 §3.1.2），不再有独立子面向字段（`subagent_system_prompt` 随 C5
    /// 移除）；子 Agent / fork / workflow 的 prompt 由同一份冻结输入按各自
    /// **执行面能力投影**重建（H2，`prompt_policy`），不再复制主冻结字节——
    /// 主链有审批 / 提问 / 子代理声明，而子链与 workflow 链没有这些持有者。
    /// `workflow_enabled` 参数随 gate 清理删除。
    ///
    /// L5：渲染面（CLAUDE.md 解析 / skills 摘要 / prompt 模板）随
    /// `FrozenSessionData::build` 留在 ACP（§0 渲染是 ACP 协议面职责），
    /// 类型经 `from_frozen_parts` 装配（peri-agent 侧不可变数据存储）。
    pub fn build_frozen_data(&self, cwd: &str) -> crate::session::executor::FrozenSessionData {
        self.build_frozen_data_with_config(&self.inner.peri_config, cwd)
    }

    /// Legacy restoration discovers configuration before admitting execution resources.
    pub(crate) fn build_frozen_data_with_config(
        &self,
        config: &crate::provider::PeriConfig,
        cwd: &str,
    ) -> crate::session::executor::FrozenSessionData {
        // 无准备输入的调用点（测试夹具 / 无执行环境的构建点）：本地探测一次并
        // 委托冻结渲染；生产准入走 `PreparedSessionInputs::build_frozen_after_activation`
        // 的有效 Workspace 来源判定（H3/D1）。
        let runtime_env = crate::prompt::PromptRuntimeEnv::detect(cwd);
        self.build_frozen_data_with_config_and_runtime(config, cwd, Some(&runtime_env))
    }

    /// `runtime_env = None` = 有效 Workspace 为显式远端（执行环境未知）：
    /// 冻结标记 unavailable，不重探本地值（H3/D1）。
    ///
    /// beta flag 投影从合并后配置派生（无来源层信息，见
    /// [`Self::build_frozen_data_with_deployment_closure`] 的口径）。
    pub(crate) fn build_frozen_data_with_config_and_runtime(
        &self,
        config: &crate::provider::PeriConfig,
        cwd: &str,
        runtime_env: Option<&crate::prompt::PromptRuntimeEnv>,
    ) -> crate::session::executor::FrozenSessionData {
        self.build_frozen_data_with_config_and_runtime_and_docs(
            config,
            cwd,
            runtime_env,
            HashMap::new(),
            // 无内容准入期的构造点（legacy 首次接纳 / 测试夹具）：没有资源面 ⇒
            // 没有 system 技能摘要（J5：不回落磁盘）。
            &[],
            // 同样没有项目指令面（X4：不回落磁盘）。
            &Default::default(),
        )
    }

    /// `skill_catalog` = 内容准入期从 system 来源（builtin `workspace` 实例）取到
    /// 的技能元数据快照（W4b/F3）；空快照 = 技能面为空（不是错误，X5），摘要随之为空。
    pub(crate) fn build_frozen_data_with_config_and_runtime_and_docs(
        &self,
        config: &crate::provider::PeriConfig,
        cwd: &str,
        runtime_env: Option<&crate::prompt::PromptRuntimeEnv>,
        docs: HashMap<String, String>,
        skill_catalog: &[peri_acp_types::skills::SkillMetadata],
        instructions: &crate::session::executor::FrozenInstructions,
    ) -> crate::session::executor::FrozenSessionData {
        let beta_flags = peri_config::betas::from_merged(config);
        self.build_frozen_data_with_deployment_closure(
            config,
            cwd,
            runtime_env,
            &beta_flags,
            docs,
            skill_catalog,
            instructions,
            &Default::default(),
        )
    }

    /// Freeze deployment absence alongside MetaHarness policy so cold restore cannot
    /// resurrect a capability that was missing when this session was created.
    #[allow(clippy::too_many_arguments)] // Mirrors the existing freeze inputs plus one immutable deployment fact.
    pub(crate) fn build_frozen_data_with_deployment_closure(
        &self,
        config: &crate::provider::PeriConfig,
        cwd: &str,
        runtime_env: Option<&crate::prompt::PromptRuntimeEnv>,
        beta_flags: &peri_acp_types::beta_flags::BetaFlags,
        docs: HashMap<String, String>,
        skill_catalog: &[peri_acp_types::skills::SkillMetadata],
        instructions: &crate::session::executor::FrozenInstructions,
        deployment_closed: &std::collections::HashSet<String>,
    ) -> crate::session::executor::FrozenSessionData {
        let frozen_date = peri_time::calendar_date(
            peri_time::now_wall(),
            peri_time::CalendarConvention::deployment_default(),
        )
        .to_string();
        let frozen_language = config.config.language.clone();
        // W5（E15/J5）：项目指令正文来自内容准入期读取的 MCP 资源快照
        // （`peri-instruction://workspace/{main|local}`）——宿主本地读盘点与
        // `@import` 解析已整体删除，任何构造点都**不回落磁盘**（X4）。
        let (claude_md, claude_local_md) = (instructions.main.clone(), instructions.local.clone());
        // W4b（F3/J1/J5）：技能摘要只从**传入的 MCP 侧元数据快照**渲染——本地
        // 扫描（原 `build_frozen_summary`）已删除，宿主不再有技能文件系统读取点。
        // 快照由调用方在内容准入期（P4）从 system 来源（builtin `workspace`
        // 实例）读取：新会话是当轮读取；legacy 首次接纳发生在准入事务之前
        // （无执行环境 ⇒ 无资源面，按 J2 §3.1 保持为空——技能仍在首轮经 MCP
        // 发现可得，只是不进冻结摘要）；恢复路径复用持久 blob，不重读。
        let skill_summary =
            peri_middlewares::SkillsMiddleware::render_frozen_summary(skill_catalog);

        let mut meta_harness_state =
            build_meta_harness_state(config.config.meta_harness.as_ref(), docs);
        meta_harness_state
            .disabled_middlewares
            .extend(deployment_closed.iter().cloned());

        // 波 4 演进（C2）：收集结果 = 执行面能力事实驱动的段落集合
        // （`prompt_policy`）——基础段（01-06 / 07_runtime / persona）与
        // language 段由 DefaultSystemPromptMiddleware / LangMiddleware 持有，
        // 链未装配的冻结渲染经同一事实源获得与装配一致的段落（决策记录 D3）；
        // 主会话能力事实来自主链装配条件投影（H2）。
        let collected =
            build_collected_sections(&meta_harness_state, None, frozen_language.as_deref());
        // L3 覆盖准入：预算 / reserved boundary token / 未知占位符 / 空覆盖 /
        // Cached 动态占位符一律拒绝应用并保留内置段（拒绝项不进入持久快照），
        // 按段落 + 错误类别记录结构化诊断。
        let rejected = crate::prompt::section_validation::sanitize_section_overrides(
            &mut meta_harness_state,
            &collected,
        );
        crate::prompt::section_validation::log_rejections(&rejected);
        let template = crate::prompt::PromptTemplate::new(&meta_harness_state, &collected);
        // 冻结运行环境随快照持久化（H3）：准入期按有效 Workspace 来源定格一次
        // ——显式远端 Workspace 为 `None`（unavailable，不冒充），此后重渲染只
        // 消费该快照。
        let frozen_runtime_env = runtime_env.map(crate::prompt::PromptRuntimeEnv::freeze);
        let env = crate::prompt::PromptEnv::frozen(cwd, &frozen_date, frozen_runtime_env.as_ref());
        let system_prompt = template.render(&env, self.inner.agent_catalog.as_ref());

        // 16_workflow 已删除（C2）：`FrozenSessionData` 无子面向字段（C5 移除），
        // 子 Agent / fork / workflow 的 prompt 在各自装配点按能力投影重建
        // （H2：`build_collected_sections_with_capabilities` +
        // `PromptEnv::frozen`），本函数只产出主链冻结 prompt。

        // 构建 v2 FrozenContext
        let v2_frozen = peri_agent::session::FrozenContext {
            system_prompt: Arc::from(system_prompt),
            external_instructions: None,
            environment_prompt: template
                .render_environment(&env, self.inner.agent_catalog.as_ref())
                .map(Arc::from),
            legacy_embedded_instructions: false,
            legacy_embedded_environment: false,
            claude_md: claude_md.map(Arc::from).unwrap_or_default(),
            skill_summary: skill_summary.map(Arc::from).unwrap_or_default(),
            date: Arc::from(frozen_date),
            language: frozen_language.map(|l| Arc::from(l.to_string())),
            meta_harness: meta_harness_state,
            // beta flag 冻结值（设计 §消费契约）：装配期从配置快照投影一次，随冻结
            // 载体传播；SubAgent/fork 复用同一份，执行路径不重读配置。
            beta_flags: beta_flags.clone(),
            builtin_assembly_mode: peri_acp_types::builtin_mcp::BuiltinAssemblyMode::SingleExit,
            runtime_env: frozen_runtime_env,
        };

        crate::session::executor::FrozenSessionData::from_frozen_parts(
            v2_frozen,
            claude_local_md.map(Arc::from),
        )
    }
}

/// Prepare a legacy session's derived environment from frozen inputs only.
/// The original main prompt and rejected V1 external-instruction provenance stay intact.
pub(crate) fn prepare_derived_environment(
    frozen: &crate::session::executor::FrozenSessionData,
    cwd: &str,
    agent_catalog: &dyn peri_acp_types::ports::AgentCatalogPort,
) -> crate::session::executor::FrozenSessionData {
    let context = frozen.v2_frozen();
    if !context.legacy_embedded_environment
        || context.legacy_embedded_instructions
        || context.environment_prompt.is_some()
    {
        return frozen.clone();
    }
    let collected = build_collected_sections(frozen.meta_harness(), None, frozen.language());
    let template = crate::prompt::PromptTemplate::new(frozen.meta_harness(), &collected);
    let env = crate::prompt::PromptEnv::frozen(cwd, frozen.date(), frozen.runtime_env());
    let mut context = context.clone();
    context.environment_prompt = template
        .render_environment(&env, agent_catalog)
        .map(Arc::from);
    crate::session::executor::FrozenSessionData::from_frozen_parts(
        context,
        frozen.claude_local_md().map(Arc::from),
    )
}

use peri_acp_types::meta_harness::{AssemblyPlan, ExecutionProfile};

pub(crate) fn build_collected_sections(
    state: &peri_acp_types::meta_harness::MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
) -> Vec<peri_agent::middleware::PromptSection> {
    let plan = AssemblyPlan::new(&state.disabled_middlewares, ExecutionProfile::Main);
    build_collected_sections_for_plan(overrides, language, &plan)
}

pub(crate) fn build_collected_sections_for_plan(
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
    plan: &AssemblyPlan,
) -> Vec<peri_agent::middleware::PromptSection> {
    peri_middlewares::prompt_policy::collect_prompt_sections(overrides, language, plan)
}

/// Render derived prompts from frozen inputs and the execution's effective skill surface.
pub(crate) fn derived_prompt_template(
    state: &peri_acp_types::meta_harness::MetaHarnessState,
    overrides: Option<&peri_acp_types::agents::AgentOverrides>,
    language: Option<&str>,
    plan: &peri_acp_types::meta_harness::AssemblyPlan,
    skill_access: peri_acp_types::skills::SkillToolAccess,
) -> crate::prompt::PromptTemplate {
    let sections = peri_middlewares::prompt_policy::collect_prompt_sections_for_tools(
        overrides,
        language,
        plan,
        skill_access,
    );
    crate::prompt::PromptTemplate::new(state, &sections)
}

/// Assemble the frozen text from the same settings that admitted this workspace.
pub(crate) fn build_meta_harness_state(
    config: Option<&HashMap<String, bool>>,
    mut docs: HashMap<String, String>,
) -> peri_acp_types::meta_harness::MetaHarnessState {
    let mut state = policy_from_settings(config);
    if let Some(config) = config {
        for &id in peri_acp_types::meta_harness::SECTION_IDS {
            if config.get(id) == Some(&true) {
                if let Some(body) = docs.remove(id) {
                    state
                        .section_overrides
                        .insert(id.to_owned(), Arc::from(body));
                } else {
                    tracing::warn!(
                        section = id,
                        "meta_harness: requested workspace override is missing"
                    );
                }
            }
        }
    }
    state
}

/// Resource admission precedes the workspace read. Evaluating switches at that
/// point must not diagnose a requested document as missing before it can arrive.
pub(crate) fn policy_from_settings(
    config: Option<&HashMap<String, bool>>,
) -> peri_acp_types::meta_harness::MetaHarnessState {
    use peri_acp_types::meta_harness::{
        MetaHarnessState, BUILT_IN_SUBAGENTS_KEY, MIDDLEWARE_NAMES,
    };

    let mut state = MetaHarnessState::default();
    let Some(config) = config else {
        return state;
    };
    for (key, enabled) in config {
        if key == BUILT_IN_SUBAGENTS_KEY {
            state.built_in_subagents_enabled = *enabled;
        } else if let Ok(capability) = peri_acp_types::builtin_mcp::capability_of_switch(key) {
            if !*enabled {
                state
                    .disabled_middlewares
                    .insert(capability.name.to_owned());
            }
            if key == capability.legacy_policy_key && config.contains_key(capability.name) {
                tracing::warn!(owner = capability.name, alias = %key,
                    "duplicate builtin switch; either spelling set to false closes the capability");
            }
        } else if MIDDLEWARE_NAMES.contains(&key.as_str()) && !*enabled {
            state.disabled_middlewares.insert(key.clone());
        }
    }
    state
}
