//! H2 对拍：**子链能力事实与真实装配结果一致**（真实 `build_subagent_middlewares`）。
//!
//! policy（`prompt_policy::subagent_chain_capabilities`）必须与子链实际装配
//! 一致——gated 段按真实持有者判定；基础段 / 语言段在子链**没有**持有者
//! （由会话冻结模板继承，policy 里的 `base_prompt` / `language` 表达继承面，
//! 不是子链声明）。

use crate::subagent::SubAgentMiddlewareConfig;
use peri_acp_types::meta_harness::{AssemblyPlan, ChainSlot, ExecutionProfile};
use peri_agent::middleware::chain::MiddlewareChain;

#[test]
fn subagent_capability_policy_matches_real_assembly() {
    for disabled in [
        Vec::<&str>::new(),
        vec!["SkillsMiddleware"],
        vec!["DefaultSystemPromptMiddleware"],
        vec!["LangMiddleware", "SkillPreloadMiddleware"],
    ] {
        let set: std::collections::HashSet<String> =
            disabled.iter().map(|key| key.to_string()).collect();
        let config = SubAgentMiddlewareConfig {
            meta_harness_disabled: set.clone(),
            ..SubAgentMiddlewareConfig::for_fork("/tmp/parity")
        };
        let mut chain = MiddlewareChain::new();
        for middleware in crate::subagent::tool::build_subagent_middlewares(config) {
            chain.add(middleware);
        }

        let plan = AssemblyPlan::new(&set, ExecutionProfile::Child);
        let sections = chain.collect_prompt_sections();
        for owner in [
            ChainSlot::Skills,
            ChainSlot::Permission,
            ChainSlot::AskUser,
            ChainSlot::SubAgent,
        ] {
            assert_eq!(
                sections.iter().any(|s| s.owner == Some(owner)),
                plan.declares_sections(owner)
            );
        }
        assert!(!chain
            .names()
            .contains(&ChainSlot::DefaultSystemPrompt.name()));
        assert_eq!(
            plan.contains(ChainSlot::DefaultSystemPrompt),
            !set.contains(ChainSlot::DefaultSystemPrompt.name())
        );
    }
}
