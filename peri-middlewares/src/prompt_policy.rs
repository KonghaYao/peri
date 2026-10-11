//! 段落能力策略（H2 单一权威）。
//!
//! 由**实际装配事实**（[`SectionCapabilities`]）派生系统提示词段落集合：
//! 主链、子链、workflow 链与冻结渲染共用本策略，ACP 不维护第二份按执行类型
//! 硬编码的排除表。
//!
//! 与 `MiddlewareChain::collect_prompt_sections` 的关系：
//! - 链已装配时，链收集结果即事实源；
//! - 无链的构造点（session/new 冻结渲染、子 Agent / workflow 的 prompt 重建）
//!   用本策略按同一批装配条件投影出等价段落，并由 parity 测试锁定
//!   （`assembly_test_sections.rs` 的主链对拍、`subagent` / `assembly::workflow`
//!   的子链 / workflow 对拍）。
//!
//! 能力缺席 ⇒ 段落消失（`docs/design/system-prompt.md`「middleware 缺席时，
//! 其工具、hook 和段落必须同时消失」）；有效模式同样参与判定（workflow 的
//! `PermissionMiddleware::disabled()` 不声明审批段）。

use std::collections::HashSet;

use peri_acp_types::{agents::AgentOverrides, meta_harness::MetaHarnessState};
use peri_agent::middleware::prompt_sections::{PromptSection, SectionCapabilities};

use crate::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use crate::hitl::HumanInTheLoopMiddleware;
use crate::permission::PermissionMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::SubAgentMiddleware;

/// 按能力事实收集段落（纯函数，渲染面与对拍测试共用）。
pub fn collect_prompt_sections(
    state: &MetaHarnessState,
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
    capabilities: &SectionCapabilities,
) -> Vec<PromptSection> {
    let mut collected = Vec::new();
    if capabilities.base_prompt {
        collected.extend(DefaultSystemPromptMiddleware::sections(overrides));
    }
    if capabilities.language {
        collected.extend(LangMiddleware::sections(language));
    }
    if capabilities.approval {
        collected.extend(PermissionMiddleware::sections_for_disabled(
            &state.disabled_middlewares,
        ));
    }
    if capabilities.ask_user {
        collected.extend(HumanInTheLoopMiddleware::sections());
    }
    if capabilities.subagent {
        collected.extend(SubAgentMiddleware::sections());
    }
    if capabilities.skills {
        collected.extend(SkillsMiddleware::sections());
    }
    collected
}

/// 主会话装配面能力事实（与 `assembly.rs` 的槽位条件一一对应）。
///
/// 主链 `PermissionMiddleware` 恒以 `with_shared_mode` 构造（broker 必填）
/// ⇒ 审批有效；`HumanInTheLoopMiddleware` 装配即持有提问通道。
/// 与真实链的对拍见 `assembly_test_sections.rs`。
pub fn main_chain_capabilities(disabled: &HashSet<String>) -> SectionCapabilities {
    SectionCapabilities {
        base_prompt: !disabled.contains("DefaultSystemPromptMiddleware"),
        language: !disabled.contains("LangMiddleware"),
        approval: !disabled.contains("PermissionMiddleware"),
        ask_user: !disabled.contains("HumanInTheLoopMiddleware"),
        subagent: !disabled.contains("SubAgentMiddleware"),
        skills: !disabled.contains("SkillsMiddleware"),
    }
}

/// 子 Agent 链能力事实（与 `build_subagent_middlewares` 的装配条件一一对应）。
///
/// 子链**没有**基础段 / 审批 / 提问 / 子代理持有者：基础段由会话冻结模板继承
/// （`base_prompt` / `language` 仍按冻结 disabled 决策表达继承面），gated 段只
/// 保留子链真实具备的 13_skills。与真实子链的对拍见
/// `subagent/tool/tool_test/sections_parity_test.rs`。
pub fn subagent_chain_capabilities(disabled: &HashSet<String>) -> SectionCapabilities {
    SectionCapabilities {
        // 基础段（01-06 / 07_runtime / persona）与语言段：继承会话冻结决策——
        // 子链无持有者，但渲染输入就是同一份冻结模板。
        base_prompt: !disabled.contains("DefaultSystemPromptMiddleware"),
        language: !disabled.contains("LangMiddleware"),
        // 子链不装配审批 / 提问 / 子代理持有者（不声明不具备的能力）。
        approval: false,
        ask_user: false,
        subagent: false,
        skills: !disabled.contains("SkillsMiddleware"),
    }
}

/// workflow agent 链能力事实（与 `build_workflow_middlewares` 的条件一一对应）。
///
/// `broker_present` / `permission_mode_present` 是装配点的实际输入：两者皆
/// Some 才构造启用审批的 `PermissionMiddleware`（有效模式），否则是
/// `PermissionMiddleware::disabled()`——存在不等于需要审批说明。workflow 链不
/// 装配 SubAgentMiddleware。与真实 workflow 链的对拍见
/// `assembly_test_workflow.rs`。
pub fn workflow_chain_capabilities(
    disabled: &HashSet<String>,
    broker_present: bool,
    permission_mode_present: bool,
) -> SectionCapabilities {
    SectionCapabilities {
        base_prompt: !disabled.contains("DefaultSystemPromptMiddleware"),
        language: !disabled.contains("LangMiddleware"),
        // 有效模式规则来自 `PermissionMiddleware`（D3 单一权威：装配与投影同源）。
        approval: !disabled.contains("PermissionMiddleware")
            && PermissionMiddleware::workflow_approval_active(
                broker_present,
                permission_mode_present,
            ),
        ask_user: !disabled.contains("HumanInTheLoopMiddleware") && broker_present,
        subagent: false,
        skills: !disabled.contains("SkillsMiddleware"),
    }
}

#[cfg(test)]
#[path = "prompt_policy_test.rs"]
mod prompt_policy_test;
