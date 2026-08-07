//! SubAgent v2 装配：从 agent_def 构造 v2-ready 数据（LLM + tools + system_prompt +
//! skill_names + max_iterations），调用方组装 [`SubagentSpawnConfig`] 后经
//! [`spawn_subagent`]（Agent 层统一入口）创建与运行。
//!
//! **P5.1 重构**：旧版本通过 `SubAgentBuilder.build()` 构造 v1 Agent，
//! 现在直接产出 v2 字段。L3：建 thread / cancel token / 事件 / 运行收尾
//! 全部移入 [`spawn_subagent`]，本模块只保留 agent_def 解析、工具过滤与
//! SandboxWrite 注入等 middlewares 能力。

use peri_agent::{
    agent::react::ReactLLM, session::subagent::SubagentCancelPolicy, tools::BaseTool,
};

use super::super::fork::allows_injected_tools;
use crate::claude_agent_parser::ClaudeAgent;

/// v2-ready SubAgent 装配产物（L3 简化：创建/运行/收尾移入 Agent 层统一入口）
pub(crate) struct AgentBuildResult {
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 过滤后的工具集（按 agent_def.tools/disallowed_tools）
    pub tools: Vec<Box<dyn BaseTool>>,
    /// SubAgent system prompt
    pub system_prompt: Option<String>,
    /// agent 定义声明的 skills（SkillPreload 装配输入）
    pub skill_names: Vec<String>,
    /// ReAct 循环最大迭代次数（来自 agent_def.max_turns，默认 200）
    pub max_iterations: usize,
}

impl super::SubAgentTool {
    /// 从 agent 定义构造 v2-ready SubAgent 数据（L3：不含 thread 创建 / 事件 /
    /// cancel token——统一入口 [`spawn_subagent`] 负责）。
    pub(crate) async fn build_agent_from_def(
        &self,
        agent_def: &ClaudeAgent,
        agent_name: &str,
        cwd: &str,
        _cancel_policy: SubagentCancelPolicy,
        _skip_events: bool,
        _setup_event_handler: bool,
    ) -> Result<AgentBuildResult, Box<dyn std::error::Error + Send + Sync>> {
        // 1. Filter tools
        let mut filtered_tools = self.filter_tools(
            &agent_def.frontmatter.tools,
            &agent_def.frontmatter.disallowed_tools,
        );

        // 显式 `tools: []` 是严格的零工具边界，禁止 WriteSandbox 等后注入工具。
        let allowed_write_dirs = &agent_def.frontmatter.allowed_write_dirs;
        if allows_injected_tools(&agent_def.frontmatter.tools) && !allowed_write_dirs.is_empty() {
            let disallowed_list = agent_def.frontmatter.disallowed_tools.to_vec();
            let is_disallowed = disallowed_list.iter().any(|n| {
                let n = n.to_lowercase();
                n == "sandboxwrite" || n == "writesandbox"
            });
            if is_disallowed {
                tracing::debug!(
                    agent_id = %agent_name,
                    "SandboxWrite 被 disallowedTools 否决，跳过注入"
                );
            } else {
                match crate::tools::filesystem::WriteSandboxTool::new(
                    cwd.to_string(),
                    allowed_write_dirs.clone(),
                ) {
                    Ok(tool) => {
                        filtered_tools.push(Box::new(tool));
                        tracing::debug!(
                            agent_id = %agent_name,
                            sandbox_dirs = ?allowed_write_dirs,
                            "SandboxWrite 工具已注入"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_name,
                            error = %e,
                            sandbox_dirs = ?allowed_write_dirs,
                            "SandboxWrite 构造失败，跳过注入"
                        );
                    }
                }
            }
        }

        tracing::debug!(
            agent_id = %agent_name,
            parent_count = self.parent_tools.len(),
            filtered_count = filtered_tools.len(),
            filtered_names = ?filtered_tools.iter().map(|t| t.name()).collect::<Vec<_>>(),
            allowed = ?agent_def.frontmatter.tools,
            disallowed = ?agent_def.frontmatter.disallowed_tools,
            "build_agent_from_def: tool filter results"
        );

        // 2. Model alias → LLM factory
        let model_alias: Option<&str> = agent_def
            .frontmatter
            .model
            .as_deref()
            .filter(|m| !m.is_empty() && *m != "inherit");
        let llm = (self.llm_factory)(model_alias);

        // 3. Max iterations
        let raw_turns = agent_def.frontmatter.max_turns.unwrap_or(200);
        let max_iterations = if raw_turns == 0 {
            200
        } else {
            raw_turns as usize
        };

        // 4. Skill names（SkillPreload 装配输入）
        let skill_names = agent_def.frontmatter.skills.clone();

        // 5. System prompt
        let system_prompt = if let Some(ref builder) = self.system_builder {
            let overrides = Self::overrides_from_agent_def(
                &agent_def.system_prompt,
                &agent_def.frontmatter.tone,
                &agent_def.frontmatter.proactiveness,
                &agent_def.frontmatter.prompt_mode,
            );
            Some(builder(overrides.as_ref(), cwd))
        } else {
            None
        };

        Ok(AgentBuildResult {
            llm,
            tools: filtered_tools,
            system_prompt,
            skill_names,
            max_iterations,
        })
    }
}
