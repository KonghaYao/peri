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
        // 调用点未准备运行环境：在此探测一次并委托冻结渲染，装配期不再各自取一份。
        let runtime_env = crate::prompt::PromptRuntimeEnv::detect(cwd);
        self.build_frozen_data_with_config_and_runtime(config, cwd, &runtime_env)
    }

    pub(crate) fn build_frozen_data_with_config_and_runtime(
        &self,
        config: &crate::provider::PeriConfig,
        cwd: &str,
        runtime_env: &crate::prompt::PromptRuntimeEnv,
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
        runtime_env: &crate::prompt::PromptRuntimeEnv,
        docs: HashMap<String, String>,
        skill_catalog: &[peri_acp_types::skills::SkillMetadata],
        instructions: &crate::session::executor::FrozenInstructions,
    ) -> crate::session::executor::FrozenSessionData {
        self.build_frozen_data_with_deployment_closure(
            config,
            cwd,
            runtime_env,
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
        runtime_env: &crate::prompt::PromptRuntimeEnv,
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
        // 冻结运行环境随快照持久化（H3）：准入期探测一次，此后重渲染只消费它。
        let frozen_runtime_env = runtime_env.freeze();
        let env = crate::prompt::PromptEnv::frozen(cwd, &frozen_date, Some(&frozen_runtime_env));
        let system_prompt = template.render(&env, self.inner.agent_catalog.as_ref());

        // 16_workflow 已删除（C2）：`FrozenSessionData` 无子面向字段（C5 移除），
        // 子 Agent / fork / workflow 的 prompt 在各自装配点按能力投影重建
        // （H2：`build_collected_sections_with_capabilities` +
        // `PromptEnv::frozen`），本函数只产出主链冻结 prompt。

        // 构建 v2 FrozenContext
        let v2_frozen = peri_agent::session::FrozenContext {
            system_prompt: Arc::from(system_prompt),
            claude_md: claude_md.map(Arc::from).unwrap_or_default(),
            skill_summary: skill_summary.map(Arc::from).unwrap_or_default(),
            date: Arc::from(frozen_date),
            language: frozen_language.map(|l| Arc::from(l.to_string())),
            meta_harness: meta_harness_state,
            runtime_env: Some(frozen_runtime_env),
        };

        crate::session::executor::FrozenSessionData::from_frozen_parts(
            v2_frozen,
            claude_local_md.map(Arc::from),
        )
    }
}
/// 渲染面段落收集：按**执行面能力事实**投影段落集合（H2 单一权威）。
///
/// 全部 `PromptTemplate` 构造点（冻结渲染 / 主重渲染 / SubAgent builder /
/// workflow fallback / workflow agent builder / 测试 helper）统一经本模块的
/// 收集入口计算收集结果，能力事实由 `peri_middlewares::prompt_policy` 的装配
/// 条件投影提供（主链 / 子链 / workflow 链各自的 `*_chain_capabilities`），与
/// 各链的真实装配结果由 parity 测试对拍。**单一事实源，禁止双轨**：这里不再
/// 有「按执行类型排除某段」的第二份名单。
///
/// 契约 3（gate 原子迁移，C2/C3 落地）：段落可见性 = 持有 middleware 是否在
/// 装配面且有效（例如 PermissionMiddleware 的 disabled 实例不声明 10_hitl）。
///
/// 落点说明（layer-imports 依赖门）：函数体直接引用 `peri_middlewares` 的
/// 策略模块——本模块为 §0 边 2 豁免的 ACP 宿主装配面
/// （`scripts/import-exemptions.conf`），渲染核心 `prompt/mod.rs` 不持有
/// middlewares 引用。
pub(crate) fn build_collected_sections(
    state: &peri_acp_types::meta_harness::MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
) -> Vec<peri_agent::middleware::PromptSection> {
    let capabilities = main_chain_capabilities(&state.disabled_middlewares);
    build_collected_sections_with_capabilities(state, overrides, language, &capabilities)
}

/// 主链能力事实（H2）：ACP 宿主装配面对 `prompt_policy` 的薄包装。
///
/// 单一权威仍在 `peri_middlewares::prompt_policy`（与真实装配对拍）；
/// 本文件是 §0 边 2 豁免的宿主装配面，middlewares 引用集中在此，
/// `host/stage_builder.rs` / `host/workflow_agent.rs` 只调用本模块。
pub(crate) fn main_chain_capabilities(
    disabled: &std::collections::HashSet<String>,
) -> peri_agent::middleware::SectionCapabilities {
    peri_middlewares::prompt_policy::main_chain_capabilities(disabled)
}

/// 子链能力事实（H2）：子链不装配审批 / 提问 / 子代理持有者等装配事实投影。
pub(crate) fn subagent_chain_capabilities(
    disabled: &std::collections::HashSet<String>,
) -> peri_agent::middleware::SectionCapabilities {
    peri_middlewares::prompt_policy::subagent_chain_capabilities(disabled)
}

/// workflow 链能力事实（H2）：含审批有效模式（broker + permission_mode 齐备）。
pub(crate) fn workflow_chain_capabilities(
    disabled: &std::collections::HashSet<String>,
    broker_present: bool,
    permission_mode_present: bool,
) -> peri_agent::middleware::SectionCapabilities {
    peri_middlewares::prompt_policy::workflow_chain_capabilities(
        disabled,
        broker_present,
        permission_mode_present,
    )
}

/// 指定执行面能力事实的段落收集（子链 / workflow 链；能力来自其装配条件投影）。
pub(crate) fn build_collected_sections_with_capabilities(
    state: &peri_acp_types::meta_harness::MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
    capabilities: &peri_agent::middleware::SectionCapabilities,
) -> Vec<peri_agent::middleware::PromptSection> {
    peri_middlewares::prompt_policy::collect_prompt_sections(
        state,
        overrides,
        language,
        capabilities,
    )
}

/// 由合并后的 meta_harness 配置与扫描结果构建冻结期 MetaHarnessState
/// （设计 §2.3；纯函数，不读盘——`build_frozen_data` 是唯一扫描入口）。
///
/// 组合规则：
/// - section + true + 文档存在 → `section_overrides`；
/// - section + true + 文档缺失 → warn + 忽略（保持内置段落）；
/// - section + false → 显式不覆盖；
/// - middleware + false → `disabled_middlewares`；
/// - middleware + true → 不放入 disabled（显式恢复）。
///
/// **v4-part-2（A7）**：`false` 键的判定面是「链槽位名 ∪ builtin 实例策略键」两表并集
/// （`MIDDLEWARE_NAMES` 只含链槽位名，`WebMiddleware` / `ArtifactMiddleware` 迁到
/// `BUILTIN_INSTANCE_POLICY_KEYS`）。只读其中一张表会让 `"WebMiddleware": false`
/// 退化为「键存在但无效果」——`disabled_middlewares` 既被链装配消费（关闭槽位），
/// 也被 builtin 关闭集消费（`closed_instances`，IF-D10），漏项即能力闭合退化
/// （ARC-CAPABILITY-CLOSURE-001）。
pub(crate) fn build_meta_harness_state(
    config: Option<&HashMap<String, bool>>,
    docs: HashMap<String, String>,
) -> peri_acp_types::meta_harness::MetaHarnessState {
    use peri_acp_types::meta_harness::{
        MetaHarnessState, BUILTIN_INSTANCE_POLICY_KEYS, BUILT_IN_SUBAGENTS_KEY, MIDDLEWARE_NAMES,
        SECTION_IDS,
    };

    let mut state = MetaHarnessState::default();
    let Some(config) = config else {
        return state;
    };
    for (key, enabled) in config {
        if SECTION_IDS.contains(&key.as_str()) {
            if *enabled {
                match docs.get(key) {
                    Some(content) => {
                        state
                            .section_overrides
                            .insert(key.clone(), Arc::from(content.as_str()));
                    }
                    None => {
                        tracing::warn!(
                            section = %key,
                            "meta_harness: section enabled but the workspace resource list \
                             has no entry for it, keeping builtin"
                        );
                    }
                }
            }
            // section + false：显式不覆盖，静默
        } else if key == BUILT_IN_SUBAGENTS_KEY {
            state.built_in_subagents_enabled = *enabled;
        } else if (MIDDLEWARE_NAMES.contains(&key.as_str())
            || BUILTIN_INSTANCE_POLICY_KEYS.contains(&key.as_str()))
            && !*enabled
        {
            state.disabled_middlewares.insert(key.clone());
            // middleware + true：显式恢复装配，静默
        }
        // 未知 key 已在解析期校验移除（provider::config::validate_meta_harness）
    }
    state
}
