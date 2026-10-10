//! 段落能力策略测试（H2）：能力事实 ↔ 段落集合，以及各执行面投影的行为。

use super::*;
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_agent::middleware::prompt_sections::SectionCapabilities;

fn state_with_disabled(disabled: &[&str]) -> MetaHarnessState {
    MetaHarnessState {
        disabled_middlewares: disabled.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn ids(sections: &[PromptSection]) -> Vec<&'static str> {
    sections.iter().map(|section| section.id).collect()
}

/// 主链默认装配：能力事实全部为真，收集结果包含基础段与全部 gated 段。
#[test]
fn main_chain_capabilities_collect_all_sections() {
    let state = MetaHarnessState::default();
    let capabilities = main_chain_capabilities(&state.disabled_middlewares);
    assert_eq!(
        capabilities,
        SectionCapabilities {
            base_prompt: true,
            language: true,
            approval: true,
            ask_user: true,
            subagent: true,
            skills: true,
        }
    );
    let collected = collect_prompt_sections(&state, None, None, &capabilities);
    let collected_ids = ids(&collected);
    for id in [
        "01_intro",
        "07_runtime",
        "persona",
        "10_hitl",
        "11_subagent",
        "12_ask_user",
        "13_skills",
    ] {
        assert!(
            collected_ids.contains(&id),
            "缺段落 {id}: {collected_ids:?}"
        );
    }
}

/// 关闭持有者 ⇒ 能力事实为假 ⇒ 对应段落消失（能力缺席即段落消失）。
#[test]
fn disabled_holders_remove_capabilities_and_sections() {
    let state = state_with_disabled(&[
        "PermissionMiddleware",
        "HumanInTheLoopMiddleware",
        "SubAgentMiddleware",
        "SkillsMiddleware",
    ]);
    let capabilities = main_chain_capabilities(&state.disabled_middlewares);
    assert!(!capabilities.approval);
    assert!(!capabilities.ask_user);
    assert!(!capabilities.subagent);
    assert!(!capabilities.skills);

    let collected = collect_prompt_sections(&state, None, None, &capabilities);
    let collected_ids = ids(&collected);
    for id in ["10_hitl", "11_subagent", "12_ask_user", "13_skills"] {
        assert!(
            !collected_ids.contains(&id),
            "关闭持有者后不得收集 {id}: {collected_ids:?}"
        );
    }
    assert!(collected_ids.contains(&"01_intro"));
}

/// H2 核心：子链没有基础段 / 审批 / 提问 / 子代理持有者——子 Agent 的 prompt
/// 不得声明 10_hitl / 11_subagent / 12_ask_user，只保留继承的基础段与子链
/// 真实具备的 13_skills（SkillsMiddleware）。
#[test]
fn subagent_capabilities_do_not_declare_absent_abilities() {
    let state = MetaHarnessState::default();
    let capabilities = subagent_chain_capabilities(&state.disabled_middlewares);
    assert!(capabilities.base_prompt, "基础段由冻结模板继承");
    assert!(capabilities.language);
    assert!(!capabilities.approval);
    assert!(!capabilities.ask_user);
    assert!(!capabilities.subagent);
    assert!(capabilities.skills);

    let collected = collect_prompt_sections(&state, None, None, &capabilities);
    let collected_ids = ids(&collected);
    assert!(collected_ids.contains(&"01_intro"));
    assert!(collected_ids.contains(&"07_runtime"));
    assert!(collected_ids.contains(&"13_skills"));
    for id in ["10_hitl", "11_subagent", "12_ask_user"] {
        assert!(
            !collected_ids.contains(&id),
            "子链不装配持有者，段落必须消失: {id} in {collected_ids:?}"
        );
    }
}

/// workflow：审批有效模式由 broker + permission_mode 决定——齐备时声明
/// 10_hitl，任一缺失（生产恒为 disabled 实例）时不声明；提问段随 broker。
#[test]
fn workflow_capabilities_follow_effective_modes() {
    let state = MetaHarnessState::default();

    let disabled_permission =
        workflow_chain_capabilities(&state.disabled_middlewares, false, false);
    assert!(!disabled_permission.approval, "disabled 实例不声明审批");
    assert!(!disabled_permission.ask_user);
    assert!(
        !disabled_permission.subagent,
        "workflow 链不装配子代理持有者"
    );

    let active = workflow_chain_capabilities(&state.disabled_middlewares, true, true);
    assert!(active.approval, "broker + mode 齐备 ⇒ 审批有效");
    assert!(active.ask_user, "broker 存在 ⇒ 提问通道装配");

    let broker_only = workflow_chain_capabilities(&state.disabled_middlewares, true, false);
    assert!(!broker_only.approval, "仅有 broker 不构成有效审批模式");
}

/// 能力事实可从实际装配的段落声明派生（链收集结果即事实源）。
#[test]
fn capabilities_derive_from_assembled_sections() {
    let state = MetaHarnessState::default();
    let declared = collect_prompt_sections(
        &state,
        None,
        None,
        &main_chain_capabilities(&state.disabled_middlewares),
    );
    let derived = SectionCapabilities::from_sections(&declared);
    assert_eq!(
        derived,
        main_chain_capabilities(&state.disabled_middlewares)
    );

    // 只有 Skills 持有者的装配面（子链形状）：派生事实与子链投影一致
    let skills_only = collect_prompt_sections(
        &state,
        None,
        None,
        &subagent_chain_capabilities(&state.disabled_middlewares),
    );
    let derived = SectionCapabilities::from_sections(&skills_only);
    assert!(derived.skills);
    assert!(!derived.approval && !derived.ask_user && !derived.subagent);
}
