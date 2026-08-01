use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::agent::LangfuseBridgeLike;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    messages::BaseMessage,
    thread::ThreadStore,
    tools::BaseTool,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{
    build_agent::CancelPolicy,
    fire_subagent_lifecycle_hooks_static, format_subagent_result,
    lifecycle::{on_subagent_stop_handler, DeregisterGuard},
};
use crate::tool_search::core_tools::TOOL_AGENT;
use crate::{
    agent_define::{AgentDefineMiddleware, AgentOverrides},
    claude_agent_parser::{parse_agent_file, ClaudeAgent, ToolsValue},
    hooks::types::{HookEvent, RegisteredHook},
    subagent::{background::BackgroundTaskRegistry, built_in_agents::get_built_in_agent},
};

/// SubAgentTool - implements the `Agent` tool, allowing LLM to delegate sub-tasks to specialized sub-agents
const AGENT_DESCRIPTION: &str = include_str!("descriptions/agent.md");

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
        Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>>,
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
    /// Frozen system prompt（session/new 时捕获，fork 路径复用以避免重建）。
    pub(crate) frozen_system_prompt: Option<Arc<String>>,
    /// bg 完成时的同步回调：在 invoke_background 路径调用 registry.complete() 之前执行
    pub(crate) on_bg_complete:
        Option<Arc<dyn Fn(&peri_agent::agent::events::BackgroundTaskResult) + Send + Sync>>,
    /// Langfuse bridge for subagent trace（None 表示遥测禁用）
    pub(crate) langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
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
            frozen_system_prompt: None,
            on_bg_complete: None,
            langfuse_bridge: None,
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
        sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>,
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

    /// 注入 main agent 捕获的 frozen system prompt。
    /// fork 路径复用以避免 system_builder 重建。
    pub fn with_frozen_system_prompt(mut self, sp: Arc<String>) -> Self {
        self.frozen_system_prompt = Some(sp);
        self
    }

    /// 设置 bg 完成时的同步回调。
    /// 在 invoke_background 路径调用 registry.complete() 之前执行。
    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<dyn Fn(&peri_agent::agent::events::BackgroundTaskResult) + Send + Sync>,
    ) -> Self {
        self.on_bg_complete = Some(cb);
        self
    }

    /// 设置 Langfuse 桥接器，用于 SubAgent 的完整 Langfuse trace。
    pub fn with_langfuse_bridge(mut self, bridge: Arc<dyn LangfuseBridgeLike>) -> Self {
        self.langfuse_bridge = Some(bridge);
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
        mode: &Option<String>,
    ) -> Option<AgentOverrides> {
        crate::subagent::fork::overrides_from_agent_def(system_prompt, tone, proactiveness, mode)
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

    fn is_direct(&self) -> bool {
        true
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

    fn aliases(&self) -> &[&str] {
        &["task"]
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
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

        // 优先读 _ctx.messages（工具调用当下的实时快照），为空时才回退到
        // self.parent_messages（SubAgentMiddleware::before_agent 时刻的旧快照）。
        //
        // Fork 需要继承当前调用现场的完整对话上下文；parent_messages 只在每轮
        // before_agent 刷新一次，若本轮中途调用 Agent(fork:true)，它会缺少本轮
        // 新增消息。
        //
        // 剪掉最后一条含 tool_calls 的 AI 消息——它包含未完成的 tool_use block（如 Agent 工具本身），
        // 缺少 tool_result 会导致 LLM API 400 错误。
        let current_messages: Vec<peri_agent::messages::BaseMessage> = {
            let mut msgs: Vec<peri_agent::messages::BaseMessage> = if !_ctx.messages.is_empty() {
                _ctx.messages.to_vec()
            } else if let Some(ref pm) = self.parent_messages {
                pm.read().clone()
            } else {
                Vec::new()
            };
            if let Some(last) = msgs.last() {
                if last.has_tool_calls() {
                    msgs.pop();
                }
            }
            msgs
        };

        if run_in_background && self.background_registry.is_some() {
            return self
                .invoke_background(prompt, subagent_type, cwd, is_fork, current_messages)
                .await;
        }

        if is_fork {
            return self.invoke_fork(&prompt, &cwd, current_messages).await;
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

        let child_thread_id = build_result.child_thread_id.clone();
        let max_iterations = build_result.max_iterations;

        // 注册到 active_agents（cancel_policy=cascade）
        if let Some(register) = &self.register_runtime {
            if let Some(ct) = &build_result.cancel_token {
                register(child_thread_id.clone(), ct.clone(), "cascade".into());
            }
        }

        // RAII guard：scope 退出时自动 deregister
        let _deregister_guard = DeregisterGuard {
            thread_id: child_thread_id.clone(),
            deregister: self.deregister_runtime.clone(),
        };

        // 组装 MiddlewareChain
        let mut chain = peri_agent::middleware::chain::MiddlewareChain::new();
        for mw in build_result.middlewares {
            chain.add(mw);
        }

        // tools: Vec<Box<dyn BaseTool>> → Vec<Arc<dyn BaseTool>>
        let tools: Vec<Arc<dyn BaseTool>> = build_result
            .tools
            .into_iter()
            .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
            .collect();

        // cancel_token（Cascade 路径必存在）
        let cancel_token = build_result
            .cancel_token
            .clone()
            .unwrap_or_else(tokio_util::sync::CancellationToken::new);

        // 构造 v2 StageContext（同步 SubAgent 不注入 parent_messages）
        // 注入 event_handler，使 SubAgent 内 LLM 的重试事件能被父级 Langfuse 追踪
        let mut llm = build_result.llm;
        llm.inject_event_handler(self.event_handler.clone());
        let v2_ctx = crate::subagent::v2_bridge::build_v2_subagent_context(
            llm,
            chain,
            tools,
            &cwd,
            cancel_token,
            Vec::new(),
            build_result.system_prompt,
            None,
            None,
            None,
            None,
            None,
            None,
        );

        // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到父 Agent 的事件处理器。让 TUI 能看到 SubAgent 内的工具调用 / AI 文本。
        // 必须在 run_react_loop 之前取出 event_handles（之后 v2_ctx.context 被 move）。
        let _forwarder_handle =
            peri_agent::agent::subagent_event_forwarder::spawn_subagent_event_forwarder(
                v2_ctx.event_handles,
                self.event_handler.clone(),
                self.langfuse_bridge.clone(),
                child_thread_id.clone(),
            );

        // push prompt 到 queue（Receive 阶段消费）
        v2_ctx
            .context
            .session
            .queue
            .push(peri_agent::session::queue::QueuedMessage::new(
                peri_agent::session::queue::MessageKind::Prompt,
                peri_agent::session::queue::MessageSource::UserInput,
                BaseMessage::human(prompt.to_string()),
            ));

        // 运行 v2 ReAct 循环
        let loop_result =
            peri_agent::agent::stages::run_react_loop(v2_ctx.context, max_iterations).await;

        let (final_text, interrupted) = match loop_result {
            peri_agent::agent::stages::LoopResult::Completed => {
                let text = extract_last_ai_text(&v2_ctx.session);
                (text, false)
            }
            peri_agent::agent::stages::LoopResult::Interrupted => (String::new(), true),
            peri_agent::agent::stages::LoopResult::Error(e) => {
                let error_summary = format!("Sub-agent execution failed: {}", e);
                let error_result: String = error_summary.chars().take(500).collect();
                on_subagent_stop_handler(
                    &self.event_handler,
                    &self.registered_hooks,
                    &self.thread_store,
                    &agent_id,
                    &child_thread_id,
                    &error_result,
                    true,
                    &cwd,
                )
                .await;
                return Err(error_summary.into());
            }
        };

        // SubagentStopped 事件 + lifecycle hook + thread_store
        let output_summary: String = if interrupted {
            "interrupted".to_string()
        } else {
            final_text.chars().take(500).collect()
        };
        on_subagent_stop_handler(
            &self.event_handler,
            &self.registered_hooks,
            &self.thread_store,
            &agent_id,
            &child_thread_id,
            &output_summary,
            interrupted,
            &cwd,
        )
        .await;

        if interrupted {
            return Ok("Sub-agent execution was interrupted".to_string());
        }

        // 复用 format_subagent_result 的格式（构造 AgentOutput）
        let output = peri_agent::agent::react::AgentOutput {
            text: final_text,
            steps: 0,
            tool_calls: Vec::new(),
            stop_reason: None,
            block_continue: None,
        };
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
}

/// 从 session transcript 提取最后一条非空 AI 消息文本
fn extract_last_ai_text(session: &Arc<peri_agent::session::Session>) -> String {
    // P1-11: 委托给 super::extract_last_ai_text（tool/mod.rs 共用实现）
    super::extract_last_ai_text(session)
}
