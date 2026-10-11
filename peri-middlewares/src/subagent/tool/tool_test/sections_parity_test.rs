//! H2 对拍：**子链能力事实与真实装配结果一致**（真实 `build_subagent_middlewares`）。
//!
//! policy（`prompt_policy::subagent_chain_capabilities`）必须与子链实际装配
//! 一致——gated 段按真实持有者判定；基础段 / 语言段在子链**没有**持有者
//! （由会话冻结模板继承，policy 里的 `base_prompt` / `language` 表达继承面，
//! 不是子链声明）。

use super::*;
use crate::prompt_policy::subagent_chain_capabilities;
use crate::subagent::SubAgentMiddlewareConfig;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::middleware::prompt_sections::SectionCapabilities;

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
        for middleware in build_subagent_middlewares(config) {
            chain.add(middleware);
        }

        let facts = SectionCapabilities::from_sections(&chain.collect_prompt_sections());
        let expected = subagent_chain_capabilities(&set);
        assert_eq!(facts.approval, expected.approval, "disabled={disabled:?}");
        assert_eq!(facts.ask_user, expected.ask_user, "disabled={disabled:?}");
        assert_eq!(facts.subagent, expected.subagent, "disabled={disabled:?}");
        assert_eq!(facts.skills, expected.skills, "disabled={disabled:?}");
        assert!(
            !facts.base_prompt && !facts.language,
            "子链不持有基础段 / 语言段：由冻结模板继承（disabled={disabled:?}）"
        );
        assert_eq!(
            expected.base_prompt,
            !set.contains("DefaultSystemPromptMiddleware"),
            "policy 的继承面由冻结 disabled 决策表达（disabled={disabled:?}）"
        );
        assert_eq!(
            expected.language,
            !set.contains("LangMiddleware"),
            "policy 的语言继承面由冻结 disabled 决策表达（disabled={disabled:?}）"
        );
    }
}
