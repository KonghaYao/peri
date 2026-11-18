//! Materialize the installed owners' static declarations before a runtime chain
//! exists. The normal chain calls the same builders on its installed instances.
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::meta_harness::{AssemblyPlan, ChainSlot};
use peri_acp_types::skills::SkillToolAccess;
use peri_agent::middleware::PromptSection;

use crate::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use crate::hitl::HumanInTheLoopMiddleware;
use crate::permission::PermissionMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::SubAgentMiddleware;

pub fn collect_prompt_sections(
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
    plan: &AssemblyPlan,
) -> Vec<PromptSection> {
    collect_prompt_sections_for_tools(overrides, language, plan, SkillToolAccess::Full)
}

pub fn collect_prompt_sections_for_tools(
    overrides: Option<&AgentOverrides>,
    language: Option<&str>,
    plan: &AssemblyPlan,
    skill_access: SkillToolAccess,
) -> Vec<PromptSection> {
    let mut sections = Vec::new();
    for &owner in ChainSlot::ALL {
        if !plan.declares_sections(owner) {
            continue;
        }
        let declared = match owner {
            ChainSlot::DefaultSystemPrompt => DefaultSystemPromptMiddleware::sections(overrides),
            ChainSlot::Lang => LangMiddleware::sections(language),
            ChainSlot::Permission => PermissionMiddleware::sections(),
            ChainSlot::AskUser => HumanInTheLoopMiddleware::sections(),
            ChainSlot::SubAgent => SubAgentMiddleware::sections(),
            ChainSlot::Skills => SkillsMiddleware::sections_for_tools(skill_access),
            _ => continue,
        };
        sections.extend(declared);
    }
    sections
}

#[cfg(test)]
#[path = "prompt_policy_test.rs"]
mod prompt_policy_test;
