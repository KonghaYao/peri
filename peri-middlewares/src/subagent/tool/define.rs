use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::{
    agent::{
        events::{AgentEvent, AgentEventHandler},
        react::{AgentInput, ReactLLM},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    thread::ThreadStore,
    tools::BaseTool,
};

use super::{
    build_agent::CancelPolicy, fire_subagent_lifecycle_hooks_static, format_subagent_result,
};
use crate::tool_search::core_tools::TOOL_AGENT;
use crate::{
    agent_define::{AgentDefineMiddleware, AgentOverrides},
    claude_agent_parser::{parse_agent_file, ClaudeAgent, ToolsValue},
    hooks::types::{HookEvent, RegisteredHook},
    subagent::{background::BackgroundTaskRegistry, built_in_agents::get_built_in_agent},
};

/// RAII guard that calls deregister on drop (panic-safe cleanup).
pub(crate) struct DeregisterGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
    }
}

/// SubAgentTool - implements the `Agent` tool, allowing LLM to delegate sub-tasks to specialized sub-agents
const AGENT_DESCRIPTION: &str = r#"Launch a sub-agent with an independent context to handle a specialized sub-task. The sub-agent executes based on the configuration defined in .claude/agents/{subagent_type}.md or .claude/agents/{subagent_type}/agent.md.

Fork mode (fork: true):
- Inherits the parent agent's full conversation history, system prompt, and tool set
- The prompt is treated as a directive within the existing context, not a standalone briefing
- Do NOT re-explain background that is already in the conversation history
- Use for tasks that require context from the ongoing conversation (e.g., continuing a multi-file refactor)
- The forked agent follows a structured output format: Scope, Result, Key files, Files changed

Usage:
- Provide a clear, self-contained task description via the prompt parameter. The sub-agent has no access to the parent conversation history
- **subagent_type is REQUIRED** unless fork=true. Specify an agent ID matching an existing agent definition file. Do NOT omit this parameter unless you intend to fork the current agent
- The sub-agent inherits the parent's tool set by default, excluding Agent itself (to prevent recursion)
- Agent definitions may restrict available tools via the tools and disallowedTools fields in frontmatter
- The sub-agent executes in isolated state — it cannot access the parent's message history or intermediate results

When to use:
- For tasks that benefit from independent context isolation (e.g., code review while working on a different feature)
- For tasks requiring specialized persona or behavior defined in agent configuration files
- For parallelizable sub-tasks that do not depend on each other's results
- When you need to break a complex task into smaller, independently executable pieces

Return format:
- If the sub-agent made tool calls, the result includes a summary of tools used followed by the final response
- If no tool calls were made, only the final response text is returned

Background execution (run_in_background: true):
- The sub-agent runs asynchronously in the background while the main agent continues
- Maximum 3 concurrent background tasks
- The main agent will be notified when the background task completes via a system message
- Use for long-running tasks that don't block the main workflow (e.g., code review, batch operations)
- Background tasks share the same working directory as the main agent"#;

pub struct SubAgentTool {
    /// Parent agent tool set (Arc shared, read-only)
    pub(crate) parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// Parent agent event handler (transparent forwarding of sub-agent events)
    pub(crate) event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// Parent agent working directory (inherited when LLM does not specify cwd)
    pub(crate) parent_cwd: String,
    /// LLM factory function, creates independent LLM instance for each sub-agent (no system, injected via with_system_prompt())
    #[allow(clippy::type_complexity)]
    pub(crate) llm_factory:
        Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    /// System prompt builder: (agent overrides, cwd) -> system prompt string
    #[allow(clippy::type_complexity)]
    pub(crate) system_builder:
        Option<Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>>,
    /// Optional cancellation token for interrupting sub-agent execution
    pub(crate) cancel: Option<AgentCancellationToken>,
    /// Shared reference to parent agent message snapshot (used by Fork path)
    pub(crate) parent_messages: Option<Arc<RwLock<Vec<BaseMessage>>>>,
    /// 后台任务注册中心（run_in_background 模式使用）
    pub(crate) background_registry: Option<Arc<BackgroundTaskRegistry>>,
    /// 子 agent 生命周期 hook（SubagentStart/SubagentStop）
    pub(crate) registered_hooks: Arc<Vec<RegisteredHook>>,
    /// Per-child event handler factory
    #[allow(clippy::type_complexity)]
    pub(crate) child_handler_factory:
        Option<Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>>,
    /// 后台任务完成事件的独立发送通道（不随 executor 生命周期销毁）
    pub(crate) bg_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>>,
    /// Thread persistence store for child threads
    pub(crate) thread_store: Option<Arc<dyn ThreadStore>>,
    /// Parent thread ID for child thread hierarchy
    pub(crate) parent_thread_id: Option<String>,
    /// Register callback: (thread_id, cancel_token, cancel_policy_str) → inserts into active_agents map
    #[allow(clippy::type_complexity)]
    pub(crate) register_runtime:
        Option<Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>>,
    /// Deregister callback: removes from active_agents map by thread_id
    pub(crate) deregister_runtime: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Frozen CLAUDE.md/AGENTS.md main content（session/new 时捕获，Arc 共享避免每轮 clone）。
    /// None 表示未配置（遗留/测试路径），SubAgent 会从磁盘读取——非生产路径。
    pub(crate) frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content。
    pub(crate) frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary。
    pub(crate) frozen_skill_summary: Option<Arc<String>>,
}

impl SubAgentTool {
    #[allow(clippy::type_complexity)]
    pub fn new(
        parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
        event_handler: Option<Arc<dyn AgentEventHandler>>,
        llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        parent_cwd: String,
    ) -> Self {
        Self {
            parent_tools,
            event_handler,
            llm_factory,
            parent_cwd,
            system_builder: None,
            cancel: None,
            parent_messages: None,
            background_registry: None,
            registered_hooks: Arc::new(Vec::new()),
            child_handler_factory: None,
            bg_event_sender: None,
            thread_store: None,
            parent_thread_id: None,
            register_runtime: None,
            deregister_runtime: None,
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn with_system_builder(
        mut self,
        builder: Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>,
    ) -> Self {
        self.system_builder = Some(builder);
        self
    }

    pub fn with_cancel(mut self, cancel: AgentCancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn with_parent_messages(mut self, messages: Arc<RwLock<Vec<BaseMessage>>>) -> Self {
        self.parent_messages = Some(messages);
        self
    }

    pub fn with_background_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.background_registry = Some(registry);
        self
    }

    pub fn with_registered_hooks(mut self, hooks: Vec<RegisteredHook>) -> Self {
        self.registered_hooks = Arc::new(hooks);
        self
    }

    #[allow(clippy::type_complexity)]
    pub fn with_child_handler_factory(
        mut self,
        factory: Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>,
    ) -> Self {
        self.child_handler_factory = Some(factory);
        self
    }

    pub fn with_bg_event_sender(
        mut self,
        sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>,
    ) -> Self {
        self.bg_event_sender = Some(sender);
        self
    }

    pub fn with_thread_store(mut self, store: Arc<dyn ThreadStore>) -> Self {
        self.thread_store = Some(store);
        self
    }

    pub fn with_parent_thread_id(mut self, id: String) -> Self {
        self.parent_thread_id = Some(id);
        self
    }

    #[allow(clippy::type_complexity)]
    pub fn with_register_runtime(
        mut self,
        cb: Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>,
    ) -> Self {
        self.register_runtime = Some(cb);
        self
    }

    pub fn with_deregister_runtime(mut self, cb: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.deregister_runtime = Some(cb);
        self
    }

    /// 注入 main agent 捕获的 frozen CLAUDE.md/Skills 数据。
    /// Arc 共享，build_tool 每轮仅 Arc::clone（无大字符串 clone）。
    pub fn with_frozen_data(
        mut self,
        claude_md: Option<Arc<String>>,
        claude_local_md: Option<Arc<String>>,
        skill_summary: Option<Arc<String>>,
    ) -> Self {
        self.frozen_claude_md = claude_md;
        self.frozen_claude_local_md = claude_local_md;
        self.frozen_skill_summary = skill_summary;
        self
    }

    pub(crate) fn load_agent_def(&self, agent_id: &str, cwd: &str) -> Result<ClaudeAgent, String> {
        let agent_path = AgentDefineMiddleware::candidate_paths(cwd, agent_id)
            .into_iter()
            .find(|p| p.is_file());

        if let Some(path) = agent_path {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Error: failed to read agent definition file: {}", e))?;
            return parse_agent_file(&content).ok_or_else(|| {
                format!(
                    "Error: failed to parse agent definition file '{}'",
                    path.display()
                )
            });
        }

        let built_in = get_built_in_agent(agent_id)
            .ok_or_else(|| format!("Error: cannot find agent definition '{}'. Check .claude/agents/ directory or use a built-in agent (explore, plan, general-purpose, verification)", agent_id))?;
        parse_agent_file(built_in.content).ok_or_else(|| {
            format!(
                "Error: failed to parse built-in agent definition '{}'",
                agent_id
            )
        })
    }

    pub(crate) fn overrides_from_agent_def(
        system_prompt: &str,
        tone: &Option<String>,
        proactiveness: &Option<String>,
    ) -> Option<AgentOverrides> {
        crate::subagent::fork::overrides_from_agent_def(system_prompt, tone, proactiveness)
    }

    pub(crate) async fn fire_subagent_lifecycle_hook(
        &self,
        event: HookEvent,
        cwd: &str,
        subagent_name: &str,
        result: Option<&str>,
    ) {
        fire_subagent_lifecycle_hooks_static(
            &self.registered_hooks,
            event,
            cwd,
            subagent_name,
            result,
        )
        .await;
    }

    pub(crate) fn filter_tools(
        &self,
        allowed: &ToolsValue,
        disallowed: &ToolsValue,
    ) -> Vec<Box<dyn BaseTool>> {
        crate::subagent::fork::filter_tools(&self.parent_tools, allowed, disallowed)
    }
}

#[async_trait]
impl BaseTool for SubAgentTool {
    fn name(&self) -> &str {
        TOOL_AGENT
    }

    fn description(&self) -> &str {
        AGENT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task description to delegate to the sub-agent. Must be clear and self-contained, as the sub-agent has no access to the parent conversation history. Include all necessary context"
                },
                "description": {
                    "type": "string",
                    "description": "A short description of the task (3-5 words), used for UI display and logging"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "The agent ID from the available agents list (e.g., 'code-reviewer', 'explorer'). Must exactly match an agent definition file at .claude/agents/{subagent_type}.md or .claude/agents/{subagent_type}/agent.md. REQUIRED unless fork=true. When not provided and fork is not set, the call will fail with an error"
                },
                "name": {
                    "type": "string",
                    "description": "A short alias for the sub-agent, used for UI identification"
                },
                "isolation": {
                    "type": "string",
                    "description": "Isolation mode for the sub-agent. Use 'worktree' to create an isolated git worktree. Currently reserved for future use"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Set to true to run the sub-agent in the background. The main agent continues immediately and receives a notification when the background task completes. Maximum 3 concurrent background tasks"
                },
                "cwd": {
                    "type": "string",
                    "description": "The working directory for the sub-agent. Defaults to inheriting the parent agent's current working directory if not specified"
                },
                "fork": {
                    "type": "boolean",
                    "description": "Set to true to fork the current agent with full conversation context. The forked agent inherits all messages, tools, and system prompt from the parent. Use when the task requires context from the ongoing conversation"
                }
            }
        })
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let prompt = match input.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return Err("Error: missing required parameter prompt".into()),
        };
        let subagent_type = input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let _description = input.get("description").and_then(|v| v.as_str());
        let _name = input.get("name").and_then(|v| v.as_str());
        let _isolation = input.get("isolation").and_then(|v| v.as_str());
        let run_in_background = input
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cwd = input
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.parent_cwd)
            .to_string();
        let is_fork = input.get("fork").and_then(|v| v.as_bool()).unwrap_or(false)
            || subagent_type.as_deref() == Some("fork");

        if run_in_background && self.background_registry.is_some() {
            return self
                .invoke_background(prompt, subagent_type, cwd, is_fork)
                .await;
        }

        if is_fork {
            return self.invoke_fork(&prompt, &cwd).await;
        }

        let agent_id = match &subagent_type {
            Some(id) => id.clone(),
            None => {
                return Err(
                    "Error: please provide subagent_type parameter to specify the agent type, or use fork: true for fork mode"
                        .into(),
                )
            }
        };

        let agent_def = match self.load_agent_def(&agent_id, &cwd) {
            Ok(a) => a,
            Err(e) => return Err(e.into()),
        };

        let build_result = self
            .build_agent_from_def(
                &agent_def,
                &agent_id,
                &cwd,
                CancelPolicy::Cascade,
                false,
                true,
            )
            .await?;

        let agent_builder = build_result.builder;
        let mut state = build_result.state;
        let child_thread_id = build_result.child_thread_id;
        let instance_id = child_thread_id.clone();
        let child_cancel = build_result.cancel_token.unwrap_or_default();

        // Register AgentRuntime: only when thread_store is present (non-legacy path)
        // Panic-safe: DeregisterGuard ensures deregister runs on drop (panic or early return)
        //
        // child_cancel is linked to parent via child_token(): parent cancel → child_cancel fires.
        // The same child_cancel is passed to execute(), so cascade cancel works correctly.
        let _deregister_guard = if self.thread_store.is_some() {
            if let Some(ref register) = self.register_runtime {
                register(
                    child_thread_id.clone(),
                    child_cancel.clone(),
                    "cascade".to_string(),
                );
            }
            DeregisterGuard {
                thread_id: child_thread_id.clone(),
                deregister: self.deregister_runtime.clone(),
            }
        } else {
            DeregisterGuard {
                thread_id: child_thread_id.clone(),
                deregister: None,
            }
        };

        tracing::info!(
            "[DEADLOCK] SubAgentTool: START child execute, agent_id={}, prompt_len={}",
            agent_id,
            prompt.len()
        );
        let exec_start = std::time::Instant::now();
        let exec_result = agent_builder
            .execute(AgentInput::text(prompt), &mut state, Some(child_cancel))
            .await;
        tracing::info!(
            "[DEADLOCK] SubAgentTool: END child execute ({:.1?}), agent_id={}, is_ok={}",
            exec_start.elapsed(),
            agent_id,
            exec_result.is_ok()
        );

        let (output_summary, stopped_is_error) = match &exec_result {
            Ok(output) => (output.text.chars().take(500).collect::<String>(), false),
            Err(e) => (
                format!("Error: {}", e)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                true,
            ),
        };
        if let Some(ref handler) = self.event_handler {
            handler.on_event(AgentEvent::SubagentStopped {
                agent_name: agent_id.clone(),
                result: output_summary.clone(),
                is_error: stopped_is_error,
                instance_id: instance_id.clone(),
            });
        }
        self.fire_subagent_lifecycle_hook(
            crate::hooks::types::HookEvent::SubagentStop,
            &cwd,
            &agent_id,
            Some(&output_summary),
        )
        .await;

        match exec_result {
            Ok(output) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store.update_thread_status(&child_thread_id, "done").await;
                }
                let result_text = format_subagent_result(&output);
                if self.thread_store.is_some() {
                    Ok(format!(
                        "child_thread_id: {}
{}",
                        child_thread_id, result_text
                    ))
                } else {
                    Ok(result_text)
                }
            }
            Err(peri_agent::error::AgentError::Interrupted) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store
                        .update_thread_status(&child_thread_id, "cancelled")
                        .await;
                }
                Ok("Sub-agent execution was interrupted".to_string())
            }
            Err(e) => {
                if let Some(ref store) = self.thread_store {
                    let _ = store.update_thread_status(&child_thread_id, "error").await;
                }
                let msg = format!("Sub-agent execution failed: {}", e);
                Err(msg.into())
            }
        }
    }
}
