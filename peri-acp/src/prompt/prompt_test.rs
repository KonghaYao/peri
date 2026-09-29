use super::*;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_middlewares::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use peri_middlewares::host_ports::AgentCatalogProvider;
use peri_middlewares::subagent::SubAgentMiddleware;
use peri_model::prompt_cache::{
    strip_system_prompt_dynamic_boundaries, SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
};
use std::sync::Arc;

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
    agent_catalog: &dyn AgentCatalogPort,
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
    template.render(&env, &features, agent_catalog)
}

fn render_cache_zones(cached: Option<&'static str>, uncached: Option<&'static str>) -> String {
    let mut collected = Vec::new();
    if let Some(content) = cached {
        collected.push(collected_section(
            "test_cached",
            PromptSectionZone::Cached,
            1,
            content,
        ));
    }
    if let Some(content) = uncached {
        collected.push(collected_section(
            "test_uncached",
            PromptSectionZone::Uncached,
            1,
            content,
        ));
    }
    PromptTemplate::new(&MetaHarnessState::default(), &collected).render(
        &PromptEnv::with_frozen_date("/tmp", "2026-01-01"),
        &PromptFeatures::none(),
        &AgentCatalogProvider::new(),
    )
}

fn collected_section(
    id: &'static str,
    zone: PromptSectionZone,
    order: u16,
    content: &'static str,
) -> PromptSection {
    PromptSection::builtin(id, zone, order, content)
}

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
        &AgentCatalogProvider::new(),
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
#[path = "prompt_overrides_test.rs"]
mod overrides_tests;
#[path = "prompt_sections_test.rs"]
mod sections_tests;
#[path = "prompt_template_test.rs"]
mod template_tests;
