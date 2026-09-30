//! Agent 定义来源解析（W5：唯一来源是 MCP `resources/read`）、本地解析与父工具策略。
//!
//! 迁移事实（plan §6.2 / §8.1 W5）：
//! - 本地三来源（project / plugin / builtin）不再由宿主读盘或读嵌入表——定义
//!   正文经 builtin `workspace` 实例的 `agent://{scope}/{id}/agent.md`
//!   `resources/read` 取得，registry 按 E13 优先级（project → builtin → plugin）
//!   选择条目；
//! - 远端 `mcp__{server}__{id}` 保持既有激活 + 内容绑定批准路径；
//! - `skills` 不再整体清空（§6.2）：条目原样留在 frontmatter，由统一 Skill
//!   activation 逐项解析/校验（见 `subagent::skill_preload`），agent 的批准不
//!   覆盖 skill；
//! - builtin 开关（`built_in_subagents_enabled`）语义与迁移前逐位一致：
//!   新建/后台路径遵守父会话冻结 policy，resume 路径允许恢复既有 builtin 定义。
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use peri_acp_types::agents::AgentOverrides;
use peri_agent::tools::BaseTool;
use peri_mcp_common::agent_definition::{ClaudeAgent, ToolsValue};
use std::collections::BTreeSet;

impl super::SubAgentTool {
    /// 新建/后台路径：builtin 来源按父会话冻结 policy 过滤。
    pub(crate) async fn load_agent_def(&self, agent_id: &str) -> Result<ClaudeAgent, String> {
        self.load_agent_def_with_built_ins(agent_id, self.built_in_subagents_enabled())
            .await
    }

    /// Resume 已有 thread 时允许恢复其原 built-in definition；新建路径遵守
    /// 父 session 冻结的 MetaHarness policy。
    pub(crate) async fn load_agent_def_for_resume(
        &self,
        agent_id: &str,
    ) -> Result<ClaudeAgent, String> {
        self.load_agent_def_with_built_ins(agent_id, true).await
    }

    pub(crate) fn built_in_subagents_enabled(&self) -> bool {
        self.parent_session
            .read()
            .as_ref()
            .map(|session| {
                session
                    .store()
                    .frozen
                    .meta_harness
                    .built_in_subagents_enabled
            })
            .unwrap_or(true)
    }

    async fn load_agent_def_with_built_ins(
        &self,
        agent_id: &str,
        include_built_ins: bool,
    ) -> Result<ClaudeAgent, String> {
        let Some(registry) = self.mcp_agent_registry.as_ref() else {
            return Err(format!(
                "Error: agent definitions are unavailable (MCP workspace face is not assembled); cannot load '{}'",
                agent_id
            ));
        };
        let activated = registry
            .activate(agent_id, include_built_ins)
            .await
            .map_err(|error| {
                format!(
                    "Error: cannot find agent definition '{}': {error}. Check .claude/agents/ directory{}",
                    agent_id,
                    if include_built_ins {
                        " or configured plugin agents"
                    } else {
                        " or configured plugin agents (built-in agents are disabled)"
                    }
                )
            })?;
        Ok(activated.definition)
    }

    /// Build retry hints from the same loader and invocation context as the failed call.
    ///
    /// Hints are resolved here, at the failure point, after the real load failed: agent
    /// sources are only known to this tool, and every enumerated id is passed through
    /// the source resolution before it can be shown to the caller.
    pub(crate) fn agent_error_with_suggestions(
        &self,
        error: &str,
        requested: Option<&str>,
    ) -> String {
        let candidates = self.loadable_agent_ids();
        if candidates.is_empty() {
            return error.to_string();
        }

        let Some(requested) = requested else {
            let available = candidates
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            return format!("{error}\nAvailable agent types: {available}");
        };

        let matches = fuzzy_rank(&candidates, requested)
            .into_iter()
            .take(3)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return error.to_string();
        }
        format!("{error}\nSuggestion: did you mean {}?", matches.join(", "))
    }

    /// Enumerate only the finite, direct agent roots understood by the real source.
    ///
    /// W5：候选来自会话级 MCP Agent registry 的本地条目（E13 优先级去重，遵守
    /// builtin 开关）——不再枚举磁盘目录：不可解析的定义不会成为建议项，与
    /// 迁移前「先枚举再经真实 loader 过滤」的净效果一致。
    fn loadable_agent_ids(&self) -> Vec<String> {
        let Some(registry) = self.mcp_agent_registry.as_ref() else {
            return Vec::new();
        };
        let mut ids: BTreeSet<String> = registry
            .local_catalog(self.built_in_subagents_enabled())
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        ids.extend(
            registry
                .entries()
                .into_iter()
                .filter(|entry| !entry.source.is_local())
                .map(|entry| entry.id),
        );
        ids.into_iter().collect()
    }

    pub(crate) fn overrides_from_agent_def(
        system_prompt: &str,
        tone: &Option<String>,
        proactiveness: &Option<String>,
        mode: &Option<String>,
    ) -> Option<AgentOverrides> {
        crate::subagent::fork::overrides_from_agent_def(system_prompt, tone, proactiveness, mode)
    }

    pub(crate) fn filter_tools(
        &self,
        allowed: &ToolsValue,
        disallowed: &ToolsValue,
    ) -> Vec<Box<dyn BaseTool>> {
        crate::subagent::fork::filter_tools(&self.parent_tools, allowed, disallowed)
    }
}

/// Skim 子序列匹配排序：返回所有可匹配候选项（score 降序）。
fn fuzzy_rank(candidates: &[String], query: &str) -> Vec<String> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| matcher.fuzzy_match(c, query).map(|s| (c.clone(), s)))
        .collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(c, _)| c).collect()
}
