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
/// 收集）始终包含（除非持有 middleware 被关闭）；环境占位符替换为运行时值。
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
    agent_catalog: &dyn AgentCatalogPort,
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String {
    let collected = crate::session::build_collected_sections(meta_harness, overrides, language);
    let template = PromptTemplate::new(meta_harness, &collected);
    let env = if let Some(date) = frozen_date {
        PromptEnv::local_probe(cwd, date)
    } else {
        PromptEnv::detect(cwd)
    };
    template.render(&env, agent_catalog)
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
        &PromptEnv::local_probe("/tmp", "2026-01-01"),
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

fn render_with_state(state: &MetaHarnessState) -> String {
    build_system_prompt(
        state,
        None,
        "/tmp",
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

// ─── H3：重渲染只消费冻结运行环境快照 ──────────────────────────────────────

/// H3：运行环境占位符取自冻结快照，不因调用时的磁盘状态（`.git`）或宿主平台
/// 漂移——同一冻结输入两次渲染字节相同。
#[test]
fn frozen_runtime_env_renders_snapshot_values_not_live_probe() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    // 会话中途出现 `.git`：冻结后渲染不得感知（快照值为 is_git_repo=false）。
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();

    let snapshot = peri_acp_types::frozen::FrozenRuntimeEnv {
        platform: "frozen-platform".to_string(),
        os_version: "frozen-os 1.0".to_string(),
        is_git_repo: false,
    };
    let state = MetaHarnessState::default();
    let collected = crate::session::build_collected_sections(&state, None, None);
    let template = PromptTemplate::new(&state, &collected);
    let catalog = AgentCatalogProvider::new();

    let first = template.render(
        &PromptEnv::frozen(cwd, "2026-01-01", Some(&snapshot)),
        &catalog,
    );
    let second = template.render(
        &PromptEnv::frozen(cwd, "2026-01-01", Some(&snapshot)),
        &catalog,
    );
    assert_eq!(first, second, "同一冻结输入两次渲染必须字节相同");
    assert!(
        first.contains("Platform: frozen-platform"),
        "平台取自冻结快照: {first}"
    );
    assert!(
        first.contains("OS Version: frozen-os 1.0"),
        "OS 版本取自冻结快照"
    );
    assert!(
        first.contains("Is directory a git repo: No"),
        "git 状态取自冻结快照（磁盘上已有 .git 也不重探）"
    );
}

/// H3 旧数据策略：快照缺少结构化环境值时渲染显式 unavailable 标记，不重探
/// 本地值冒充（也不留空）。
#[test]
fn unavailable_runtime_env_renders_explicit_marker() {
    let state = MetaHarnessState::default();
    let collected = crate::session::build_collected_sections(&state, None, None);
    let rendered = PromptTemplate::new(&state, &collected).render(
        &PromptEnv::frozen("/tmp", "2026-01-01", None),
        &AgentCatalogProvider::new(),
    );
    assert!(
        rendered.contains(RUNTIME_ENV_UNAVAILABLE),
        "缺失冻结环境必须显式标记: {rendered}"
    );
    assert!(
        !rendered.contains("Platform: macos") && !rendered.contains("Platform: linux"),
        "不得用本地探测值冒充历史环境"
    );
}
