//! 冻结期 prompt、skills 与 MetaHarness 渲染装配。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::agents::AgentOverrides;

use super::SessionManager;

impl SessionManager {
    /// 构建会话级 frozen 数据（统一构造入口，消除 TUI/stdio 重复 5 处）。
    ///
    /// 波 4 演进（C2/C5）：16_workflow 已整段删除（ultracode skill 完整覆盖，
    /// 设计 §3.1.2），子面向 prompt 与主 prompt 字节相同——不再二次渲染；
    /// `subagent_system_prompt` 字段已随 C5 移除，子面向直接复用
    /// `system_prompt()`；`workflow_enabled` 参数随 gate 清理删除。
    ///
    /// L5：渲染面（CLAUDE.md 解析 / skills 摘要 / prompt 模板）随
    /// `FrozenSessionData::build` 留在 ACP（§0 渲染是 ACP 协议面职责），
    /// 类型经 `from_frozen_parts` 装配（peri-agent 侧不可变数据存储）。
    pub fn build_frozen_data(
        &self,
        cwd: &str,
        plugin_skill_roots: &[peri_acp_types::skills::SkillRoot],
        plugin_agent_dirs: &[std::path::PathBuf],
    ) -> crate::session::executor::FrozenSessionData {
        let frozen_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let frozen_language = self.inner.peri_config.config.language.clone();
        let (claude_md, claude_local_md) =
            peri_middlewares::AgentsMdMiddleware::read_frozen_content(cwd);
        // 一次性读取 disableBundledSkills 并冻结到 frozen_skill_summary
        // （保持系统提示词稳定性：会话内不重读）
        let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
        let skill_summary = peri_middlewares::SkillsMiddleware::build_frozen_summary(
            cwd,
            plugin_skill_roots.to_vec(),
            disable_bundled,
        );

        // MetaHarness（设计 §2.3）：冻结期一次读取合并后的 settings + 扫描
        // `.peri/meta/*.md`，构建状态后随冻结载体传播；主 prompt 与 SubAgent
        // 无 workflow 版共用同一状态（同源一致性，防双轨不一致）。
        let meta_harness_state = build_meta_harness_state(
            self.inner.peri_config.config.meta_harness.as_ref(),
            peri_middlewares::meta_harness::scan_harness_docs(cwd),
        );

        let features = crate::prompt::PromptFeatures::detect();
        // 波 4 演进（C2）：收集结果 = 渲染面静态声明（冻结 disabled 集合 +
        // overrides + 冻结语言驱动，`build_collected_sections`）——基础段
        // （01-06 / 07_runtime / persona）与 language 段由
        // DefaultSystemPromptMiddleware / LangMiddleware 持有，链未装配的
        // 冻结渲染经同一事实源获得与装配一致的段落（决策记录 D3）。
        let collected =
            build_collected_sections(&meta_harness_state, None, frozen_language.as_deref());
        let template = crate::prompt::PromptTemplate::new(&meta_harness_state, &collected);
        let env = crate::prompt::PromptEnv::with_frozen_date(cwd, &frozen_date);
        let system_prompt = template.render(
            &env,
            &features,
            self.inner.skills.as_ref(),
            plugin_agent_dirs,
        );

        // 16_workflow 已删除（C2）：子面向 prompt 与主 prompt 字节相同，
        // 不再二次渲染——`FrozenSessionData` 无子面向字段（C5 移除），
        // 子 agent / fork / workflow agent 直接复用 `system_prompt()`。

        // 构建 v2 FrozenContext
        let v2_frozen = peri_agent::session::FrozenContext {
            system_prompt: Arc::from(system_prompt),
            claude_md: claude_md.map(Arc::from).unwrap_or_default(),
            skill_summary: skill_summary.map(Arc::from).unwrap_or_default(),
            date: Arc::from(frozen_date),
            language: frozen_language.map(|l| Arc::from(l.to_string())),
            meta_harness: meta_harness_state,
        };

        crate::session::executor::FrozenSessionData::from_frozen_parts(
            v2_frozen,
            claude_local_md.map(Arc::from),
        )
    }
}
/// 渲染面收集结果静态声明（波 4 演进 2，决策记录 D3 / C3 D4）。
///
/// 全部 `PromptTemplate` 构造点（冻结渲染 / 主重渲染 / SubAgent builder /
/// workflow fallback / workflow agent builder / 测试 helper）统一经本函数
/// 计算收集结果：`DefaultSystemPromptMiddleware` / `LangMiddleware` 与
/// gated 段持有者（`HumanInTheLoopMiddleware` / `SubAgentMiddleware` /
/// `SkillsMiddleware`）不在 `state.disabled_middlewares` 时收集对应段落。
/// 与链侧 `MiddlewareChain::collect_prompt_sections()` 调用同一段声明函数
/// （`peri_middlewares` 各持有者）——**单一事实源，禁止双轨**；冻结状态
/// 驱动使链未装配的构造点（如 session/new 冻结渲染）也能得到与装配一致
/// 的段落（ARC-FROZEN-001 语义保持）。
///
/// 契约 3（gate 原子迁移，C2/C3 落地）：全部迁移段 gate = 持有 middleware
/// 是否在链上——本函数按冻结 disabled 集合判定装配面，收集即装配。
///
/// 落点说明（layer-imports 依赖门）：函数体直接引用 `peri_middlewares`
/// 各持有者的段声明——本模块为 §0 边 2 豁免的 ACP 宿主装配面
/// （`scripts/import-exemptions.conf`，与 `scan_harness_docs` 同模式），
/// 渲染核心 `prompt/mod.rs` 不持有 middlewares 引用。
pub(crate) fn build_collected_sections(
    state: &peri_acp_types::meta_harness::MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
) -> Vec<peri_agent::middleware::PromptSection> {
    let mut collected = Vec::new();
    if !state
        .disabled_middlewares
        .contains("DefaultSystemPromptMiddleware")
    {
        collected.extend(
            peri_middlewares::default_system_prompt::DefaultSystemPromptMiddleware::sections(
                overrides,
            ),
        );
    }
    if !state.disabled_middlewares.contains("LangMiddleware") {
        collected
            .extend(peri_middlewares::default_system_prompt::LangMiddleware::sections(language));
    }
    if !state.disabled_middlewares.contains("PermissionMiddleware") {
        collected.extend(peri_middlewares::permission::PermissionMiddleware::sections());
    }
    if !state
        .disabled_middlewares
        .contains("HumanInTheLoopMiddleware")
    {
        collected.extend(peri_middlewares::hitl::HumanInTheLoopMiddleware::sections());
    }
    if !state.disabled_middlewares.contains("SubAgentMiddleware") {
        collected.extend(peri_middlewares::subagent::SubAgentMiddleware::sections());
    }
    if !state.disabled_middlewares.contains("SkillsMiddleware") {
        collected.extend(peri_middlewares::skills::SkillsMiddleware::sections());
    }
    collected
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
pub(super) fn build_meta_harness_state(
    config: Option<&HashMap<String, bool>>,
    docs: HashMap<String, String>,
) -> peri_acp_types::meta_harness::MetaHarnessState {
    use peri_acp_types::meta_harness::{
        MetaHarnessState, BUILT_IN_SUBAGENTS_KEY, MIDDLEWARE_NAMES, SECTION_IDS,
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
                            "meta_harness: section enabled but no .peri/meta/{key}.md, keeping builtin"
                        );
                    }
                }
            }
            // section + false：显式不覆盖，静默
        } else if key == BUILT_IN_SUBAGENTS_KEY {
            state.built_in_subagents_enabled = *enabled;
        } else if MIDDLEWARE_NAMES.contains(&key.as_str()) && !*enabled {
            state.disabled_middlewares.insert(key.clone());
            // middleware + true：显式恢复装配，静默
        }
        // 未知 key 已在解析期校验移除（provider::config::validate_meta_harness）
    }
    state
}
