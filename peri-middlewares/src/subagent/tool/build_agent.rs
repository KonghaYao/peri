use std::sync::Arc;

use peri_agent::{
    agent::{
        events::AgentEvent, react::ReactLLM, state::AgentState, AgentCancellationToken, ReActAgent,
    },
    thread::ThreadMeta,
};

use super::{build_subagent_middlewares, SourceAgentIdHandler};
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

/// Concrete builder type for subagents.
type SubReActAgent = ReActAgent<Box<dyn ReactLLM + Send + Sync>, AgentState>;

/// Result of building a subagent from an agent definition.
/// The caller handles execute, result handling, and cleanup.
pub(crate) struct AgentBuildResult {
    pub builder: SubReActAgent,
    pub state: AgentState,
    pub child_thread_id: String,
    pub cancel_token: Option<AgentCancellationToken>,
}

impl super::SubAgentTool {
    /// Build a ReActAgent + AgentState from an agent definition.
    ///
    /// `skip_events`: if true, SubagentStarted/SubagentStart events are NOT emitted here
    /// (used by background path which emits them later in tokio::spawn).
    /// `setup_event_handler`: if true, sets up child_handler_factory or event_handler
    /// with the generated child_thread_id as instance_id (normal path). If false (background
    /// path), no event handler is configured here.
    pub(crate) async fn build_agent_from_def(
        &self,
        agent_def: &ClaudeAgent,
        agent_name: &str,
        cwd: &str,
        cancel_policy: CancelPolicy,
        skip_events: bool,
        setup_event_handler: bool,
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
            // 强类型枚举赋值：经 FromStr 解析本地的 cancel_policy 字符串
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
        let filtered_tools = self.filter_tools(
            &agent_def.frontmatter.tools,
            &agent_def.frontmatter.disallowed_tools,
        );

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

        // 6. Build agent
        let mut agent_builder = ReActAgent::new(llm).max_iterations(max_iterations);

        // 7. Middlewares
        for mw in build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
            agent_def.frontmatter.skills.clone(),
            cwd,
        )) {
            agent_builder = agent_builder.add_middleware(mw);
        }

        // 8. System prompt
        if let Some(ref builder) = self.system_builder {
            let overrides = Self::overrides_from_agent_def(
                &agent_def.system_prompt,
                &agent_def.frontmatter.tone,
                &agent_def.frontmatter.proactiveness,
            );
            let system_content = builder(overrides.as_ref(), cwd);
            agent_builder = agent_builder.with_system_prompt(system_content);
        }

        // 9. Register tools
        for tool in filtered_tools {
            agent_builder = agent_builder.register_tool(tool);
        }

        // 10. Event handler
        if setup_event_handler {
            if let Some(ref factory) = self.child_handler_factory {
                agent_builder = agent_builder.with_event_handler(factory(child_thread_id.clone()));
            } else if let Some(handler) = &self.event_handler {
                let tagged = Arc::new(SourceAgentIdHandler::new(
                    Arc::clone(handler),
                    child_thread_id.clone(),
                ));
                agent_builder = agent_builder.with_event_handler(tagged);
            }
        }

        // 11. Agent state
        let state = if let Some(ref store) = self.thread_store {
            AgentState::new(cwd.to_string())
                .with_persistence(Arc::clone(store), child_thread_id.clone())
        } else {
            AgentState::new(cwd.to_string())
        };

        // 12-13. Cancel token (cascade → child_token, independent → new)
        let cancel_token = match cancel_policy {
            CancelPolicy::Cascade => self.cancel.as_ref().map(|t| t.child_token()),
            CancelPolicy::Independent => Some(AgentCancellationToken::new()),
        };

        // 14. Events (skip if background path)
        if !skip_events {
            if let Some(ref handler) = self.event_handler {
                handler.on_event(AgentEvent::SubagentStarted {
                    agent_name: agent_name.to_string(),
                    instance_id: child_thread_id.clone(),
                    is_background: false,
                });
            }
            self.fire_subagent_lifecycle_hook(HookEvent::SubagentStart, cwd, agent_name, None)
                .await;
        }

        Ok(AgentBuildResult {
            builder: agent_builder,
            state,
            child_thread_id,
            cancel_token,
        })
    }
}
