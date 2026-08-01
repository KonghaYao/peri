//! SubAgent v2 装配：从 agent_def 构造 v2-ready 数据（LLM + middlewares + tools +
//! cancel_token + child_thread_id + max_iterations + system_prompt），调用方直接
//! 喂给 `build_v2_subagent_context` + `run_react_loop`。
//!
//! **P5.1 重构**：旧版本通过 `SubAgentBuilder.build()` 构造 v1 Agent，
//! 现在直接产出 v2 字段。

use peri_agent::{
    agent::react::ReactLLM, middleware::r#trait::Middleware, thread::ThreadMeta, tools::BaseTool,
};
use tokio_util::sync::CancellationToken;

use super::super::fork::allows_injected_tools;
use super::build_subagent_middlewares;
use crate::{
    claude_agent_parser::ClaudeAgent, hooks::types::HookEvent, subagent::SubAgentMiddlewareConfig,
};

/// Controls how parent cancellation affects child agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelPolicy {
    /// Parent cancel → child cancel (normal sync, fork)
    Cascade,
    /// Only session-level cancel_all_agents can stop this (background)
    Independent,
}

/// v2-ready SubAgent 装配产物
pub(crate) struct AgentBuildResult {
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 已组装的中间件（含 frozen CLAUDE.md / Skills / TodoMiddleware）
    pub middlewares: Vec<Box<dyn Middleware>>,
    /// 过滤后的工具集（按 agent_def.tools/disallowed_tools）
    pub tools: Vec<Box<dyn BaseTool>>,
    /// SubAgent system prompt
    pub system_prompt: Option<String>,
    /// 子 agent 唯一标识（thread_id / instance_id）
    pub child_thread_id: String,
    /// 可选 cancel token（Cascade = parent.child_token()，Independent = new）
    pub cancel_token: Option<CancellationToken>,
    /// ReAct 循环最大迭代次数（来自 agent_def.max_turns，默认 200）
    pub max_iterations: usize,
}

impl super::SubAgentTool {
    /// 从 agent 定义构造 v2-ready SubAgent 数据。
    ///
    /// `skip_events`: if true, SubagentStarted/SubagentStart events are NOT emitted here
    /// (used by background path which emits them later in tokio::spawn).
    /// `setup_event_handler`: if true, sets up child_handler_factory or event_handler
    /// with the generated child_thread_id as instance_id (normal path). If false (background
    /// path), no event handler is configured here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_agent_from_def(
        &self,
        agent_def: &ClaudeAgent,
        agent_name: &str,
        cwd: &str,
        cancel_policy: CancelPolicy,
        skip_events: bool,
        _setup_event_handler: bool,
    ) -> Result<AgentBuildResult, Box<dyn std::error::Error + Send + Sync>> {
        // 1. Generate child_thread_id
        let child_thread_id = uuid::Uuid::now_v7().to_string();

        // 2. Thread store setup
        if let Some(ref store) = self.thread_store {
            let cancel_policy_str = match cancel_policy {
                CancelPolicy::Cascade => "cascade".to_string(),
                CancelPolicy::Independent => "independent".to_string(),
            };
            let mut child_meta = ThreadMeta::new(cwd);
            child_meta.id = child_thread_id.clone();
            child_meta.parent_thread_id = self.parent_thread_id.clone();
            child_meta.hidden = true;
            child_meta.cancel_policy = cancel_policy_str
                .parse()
                .expect("cancel_policy_str 由本枚举构造，解析不会失败");
            child_meta.title = Some(agent_name.to_string());
            store
                .create_thread(child_meta)
                .await
                .map_err(|e| format!("Failed to create child thread: {}", e))?;
        }

        // 3. Filter tools
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

        // 4. Model alias → LLM factory
        let model_alias: Option<&str> = agent_def
            .frontmatter
            .model
            .as_deref()
            .filter(|m| !m.is_empty() && *m != "inherit");
        let llm = (self.llm_factory)(model_alias);

        // 5. Max iterations
        let raw_turns = agent_def.frontmatter.max_turns.unwrap_or(200);
        let max_iterations = if raw_turns == 0 {
            200
        } else {
            raw_turns as usize
        };

        // 6. Middlewares
        let mw_config =
            SubAgentMiddlewareConfig::for_agent_def(agent_def.frontmatter.skills.clone(), cwd)
                .with_frozen(
                    self.frozen_claude_md
                        .as_deref()
                        .map(|s| s.as_str().to_string()),
                    self.frozen_claude_local_md
                        .as_deref()
                        .map(|s| s.as_str().to_string()),
                    self.frozen_skill_summary
                        .as_deref()
                        .map(|s| s.as_str().to_string()),
                );
        let middlewares = build_subagent_middlewares(mw_config);

        // 7. System prompt
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

        // 8. Cancel token
        let cancel_token: Option<CancellationToken> = match cancel_policy {
            CancelPolicy::Cascade => self.cancel.as_ref().map(|t| t.child_token()),
            CancelPolicy::Independent => Some(CancellationToken::new()),
        };

        // 9. Events (skip if background path)
        if !skip_events {
            if let Some(ref handler) = self.event_handler {
                handler.on_event(peri_agent::agent::events::ExecutorEvent::SubagentStarted {
                    agent_name: agent_name.to_string(),
                    instance_id: child_thread_id.clone(),
                    is_background: false,
                });
            }
            self.fire_subagent_lifecycle_hook(HookEvent::SubagentStart, cwd, agent_name, None)
                .await;
        }

        Ok(AgentBuildResult {
            llm,
            middlewares,
            tools: filtered_tools,
            system_prompt,
            child_thread_id,
            cancel_token,
            max_iterations,
        })
    }
}
