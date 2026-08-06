use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::session::subagent::{
    SessionFactory, SubagentHost, SubagentLifecycleStart, SubagentLifecycleStop,
    SubagentSpawnConfig, SubagentSpawned,
};
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    messages::BaseMessage,
    tools::BaseTool,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::{fire_subagent_lifecycle_hooks_static, SubagentChainAssemblerImpl};
use crate::tool_search::core_tools::TOOL_AGENT;
use crate::tool_search::ExecuteExtraToolResolver;
use crate::{
    agent_define::{AgentDefineMiddleware, AgentOverrides},
    claude_agent_parser::{parse_agent_file, ClaudeAgent, ToolsValue},
    hooks::types::RegisteredHook,
    subagent::built_in_agents::get_built_in_agent,
};

/// SubAgentTool - implements the `Agent` tool, allowing LLM to delegate sub-tasks to specialized sub-agents
const AGENT_DESCRIPTION: &str = include_str!("descriptions/agent.md");

/// SubAgentTool（L3 瘦身）：只声明工具与发起意图，不持有创建实现。
///
/// 创建（建 thread / 建 session / 运行 / 收尾）统一经
/// [`spawn_subagent`]（peri-agent `SessionFactory` 统一入口）。父侧运行时通道
/// （thread_store / task_manager / bg 事件 / register / deregister / frozen
/// 回退值）聚合在 [`SubagentHost`]；生产路径经 `parent_session` 的 host 读取
/// （builder 在主 session 创建后注入），测试/遗留路径经 `with_*` 直接注入
/// tool 的 host 回退。
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
    /// 子 agent 生命周期 hook（SubagentStart/SubagentStop；构造 lifecycle 闭包用）
    pub(crate) registered_hooks: Arc<Vec<RegisteredHook>>,
    /// Per-child event handler factory
    #[allow(clippy::type_complexity)]
    pub(crate) child_handler_factory:
        Option<Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>>,
    /// 父 agent 的 v2 事件侧 AgentId（共享 cell，由 peri-acp builder 在
    /// 主 v2 session 创建后注入；None = 未注入/测试路径 → 不 emit v2 Start/Stop）。
    pub(crate) parent_agent_id: Arc<RwLock<Option<AgentId>>>,
    /// 父 v2 session（L3）：builder 在主 session 创建后注入；运行时通道经
    /// `parent_session.subagent_host()` 读取。
    pub(crate) parent_session: Arc<RwLock<Option<Arc<peri_agent::session::Session>>>>,
    /// 运行时通道回退值（测试/遗留路径经 with_* 注入；生产路径为默认空，
    /// 由 parent_session 的 host 覆盖）
    pub(crate) host: SubagentHost,
    /// 子链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub(crate) chain_assembler: Arc<dyn peri_agent::session::subagent::SubagentChainAssembler>,
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
            registered_hooks: Arc::new(Vec::new()),
            child_handler_factory: None,
            parent_agent_id: Arc::new(RwLock::new(None)),
            parent_session: Arc::new(RwLock::new(None)),
            host: SubagentHost::default(),
            chain_assembler: Arc::new(SubagentChainAssemblerImpl),
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

    /// 注入父 agent 事件侧 AgentId 共享 cell（与 SubAgentMiddleware 同一 Arc）。
    pub(crate) fn with_parent_agent_id(mut self, cell: Arc<RwLock<Option<AgentId>>>) -> Self {
        self.parent_agent_id = cell;
        self
    }

    /// 注入父 v2 session（L3）：builder 在主 session 创建后调用。
    pub(crate) fn with_parent_session(self, session: Arc<peri_agent::session::Session>) -> Self {
        *self.parent_session.write() = Some(session);
        self
    }

    // ── 运行时通道回退注入（测试/遗留路径；生产路径经 parent_session 的 host） ──

    pub fn with_task_manager(
        mut self,
        task_manager: Arc<peri_agent::agent::async_tasks::TaskManager>,
    ) -> Self {
        self.host.task_manager = Some(task_manager);
        self
    }

    pub fn with_bg_event_sender(
        mut self,
        sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>,
    ) -> Self {
        self.host.bg_event_sender = Some(sender);
        self
    }

    pub fn with_thread_store(mut self, store: Arc<dyn peri_agent::thread::ThreadStore>) -> Self {
        self.host.thread_store = Some(store);
        self
    }

    pub fn with_parent_thread_id(mut self, id: String) -> Self {
        self.host.parent_thread_id = Some(id);
        self
    }

    #[allow(clippy::type_complexity)]
    pub fn with_register_runtime(
        mut self,
        cb: Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>,
    ) -> Self {
        self.host.register_runtime = Some(cb);
        self
    }

    pub fn with_deregister_runtime(mut self, cb: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.host.deregister_runtime = Some(cb);
        self
    }

    /// 注入 main agent 捕获的 frozen CLAUDE.md/Skills 数据（测试/遗留回退；
    /// 生产路径 frozen 数据由 [`spawn_subagent`] 从 parent session copy）。
    pub fn with_frozen_data(
        mut self,
        claude_md: Option<Arc<String>>,
        claude_local_md: Option<Arc<String>>,
        skill_summary: Option<Arc<String>>,
    ) -> Self {
        self.host.frozen_claude_md = claude_md;
        self.host.frozen_claude_local_md = claude_local_md;
        self.host.frozen_skill_summary = skill_summary;
        self
    }

    /// 注入 main agent 捕获的 frozen system prompt（fork 路径复用以避免重建）。
    pub fn with_frozen_system_prompt(mut self, sp: Arc<String>) -> Self {
        self.host.frozen_system_prompt = Some(sp);
        self
    }

    /// 设置 bg 完成时的同步回调（测试/遗留回退；生产路径经 parent_session 的 host）。
    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<
            dyn Fn(
                    &peri_agent::agent::events::BackgroundTaskResult,
                    peri_agent::agent::async_tasks::BgTaskKind,
                ) + Send
                + Sync,
        >,
    ) -> Self {
        self.host.on_bg_complete = Some(cb);
        self
    }

    /// 设置 Langfuse 桥接器（测试/遗留回退；生产路径经 parent_session 的 host）。
    pub fn with_langfuse_bridge(
        mut self,
        bridge: Arc<dyn peri_agent::agent::LangfuseBridgeLike>,
    ) -> Self {
        self.host.langfuse_bridge = Some(bridge);
        self
    }

    /// 父侧运行时通道（生产路径：parent_session 的 host；测试/遗留：tool 自身 host 回退）。
    pub(crate) fn host(&self) -> Option<Arc<SubagentHost>> {
        self.parent_session
            .read()
            .as_ref()
            .and_then(|s| s.subagent_host())
            .or_else(|| Some(Arc::new(self.host.clone())))
    }

    /// 生命周期 hook 闭包（middlewares 构造：内部触发 RegisteredHook；
    /// registered_hooks 为空时不构造闭包）。
    pub(crate) fn lifecycle_closures(
        &self,
    ) -> (
        Option<SubagentLifecycleStart>,
        Option<SubagentLifecycleStop>,
    ) {
        if self.registered_hooks.is_empty() {
            return (None, None);
        }
        let hooks_start = self.registered_hooks.clone();
        let on_subagent_start: Option<SubagentLifecycleStart> =
            Some(Arc::new(move |name: &str, cwd: &str| {
                let hooks = hooks_start.clone();
                let name = name.to_string();
                let cwd = cwd.to_string();
                tokio::spawn(async move {
                    fire_subagent_lifecycle_hooks_static(
                        &hooks,
                        crate::hooks::types::HookEvent::SubagentStart,
                        &cwd,
                        &name,
                        None,
                    )
                    .await;
                });
            }));
        let hooks_stop = self.registered_hooks.clone();
        let on_subagent_stop: Option<SubagentLifecycleStop> = Some(Arc::new(
            move |name: &str, cwd: &str, result: &str, is_error: bool| {
                let hooks = hooks_stop.clone();
                let name = name.to_string();
                let cwd = cwd.to_string();
                let result = result.to_string();
                tokio::spawn(async move {
                    fire_subagent_lifecycle_hooks_static(
                        &hooks,
                        crate::hooks::types::HookEvent::SubagentStop,
                        &cwd,
                        &name,
                        Some(&result),
                    )
                    .await;
                });
                let _ = is_error; // SubagentStop hook 不区分 error/正常
            },
        ));
        (on_subagent_start, on_subagent_stop)
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

    pub(crate) fn filter_tools(
        &self,
        allowed: &ToolsValue,
        disallowed: &ToolsValue,
    ) -> Vec<Box<dyn BaseTool>> {
        crate::subagent::fork::filter_tools(&self.parent_tools, allowed, disallowed)
    }

    /// 组装 [`SubagentSpawnConfig`] 的公共部分（父侧通道 + 意图骨架）。
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    pub(crate) fn spawn_config_base(
        &self,
        agent_name: String,
        prompt: String,
        parent_messages: Vec<BaseMessage>,
        cancel_policy: peri_agent::session::subagent::SubagentCancelPolicy,
        max_iterations: usize,
        fork_directive_kind: Option<peri_agent::session::subagent::ForkDirectiveKind>,
        run_mode: peri_agent::session::subagent::SubagentRunMode,
        llm: Box<dyn ReactLLM + Send + Sync>,
        tools: Vec<Arc<dyn BaseTool>>,
        system_prompt: Option<String>,
        skill_names: Vec<String>,
        cwd: String,
    ) -> SubagentSpawnConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
        SubagentSpawnConfig {
            agent_name,
            prompt,
            parent_messages,
            cancel_policy,
            max_iterations,
            fork_directive_kind,
            run_mode,
            skill_names,
            llm,
            chain_assembler: Arc::clone(&self.chain_assembler),
            tools,
            system_prompt,
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store: host.as_ref().and_then(|h| h.thread_store.clone()),
            event_handler: self.event_handler.clone(),
            bg_event_sender: host.as_ref().and_then(|h| h.bg_event_sender.clone()),
            task_manager: host.as_ref().and_then(|h| h.task_manager.clone()),
            on_bg_complete: host.as_ref().and_then(|h| h.on_bg_complete.clone()),
            langfuse_bridge: host.as_ref().and_then(|h| h.langfuse_bridge.clone()),
            on_subagent_start,
            on_subagent_stop,
            register_runtime: host.as_ref().and_then(|h| h.register_runtime.clone()),
            deregister_runtime: host.as_ref().and_then(|h| h.deregister_runtime.clone()),
            parent_agent_id: *self.parent_agent_id.read(),
            // 父侧数据回退（parent session 存在时由 spawn_subagent 覆盖）
            cancel_token: self.cancel.clone(),
            cwd: Some(cwd),
            parent_thread_id: host.as_ref().and_then(|h| h.parent_thread_id.clone()),
            frozen_claude_md: host
                .as_ref()
                .and_then(|h| h.frozen_claude_md.as_deref().map(|s| s.to_string())),
            frozen_claude_local_md: host
                .as_ref()
                .and_then(|h| h.frozen_claude_local_md.as_deref().map(|s| s.to_string())),
            frozen_skill_summary: host
                .as_ref()
                .and_then(|h| h.frozen_skill_summary.as_deref().map(|s| s.to_string())),
            frozen_date: None,
        }
    }

    /// 调用统一入口（parent 存在时 frozen/thread 父子链自 parent session 读取）。
    pub(crate) async fn spawn(
        &self,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        let parent = self.parent_session.read().clone();
        SessionFactory::spawn_subagent(parent.as_ref(), config).await
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
                    "description": "The agent ID from the available agents list (e.g., 'code-reviewer', 'explorer'). Must exactly match an agent definition file at .claude/agents/{subagent_type}.md or .claude/agents/{subagent_type}/agent.md. REQUIRED unless fork=true. When not provided and fork is not set, the call will fail"
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

        // 后台路径需要 task_manager（L3：经 parent_session 的 host 或 tool host 回退）
        let host = self.host();
        if run_in_background && host.as_ref().and_then(|h| h.task_manager.clone()).is_some() {
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
                peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
                false,
                true,
            )
            .await?;

        let llm = build_result.llm;

        let config = self.spawn_config_base(
            agent_id.clone(),
            prompt.clone(),
            Vec::new(),
            peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
            build_result.max_iterations,
            None, // agent 定义路径不包装 fork directive
            peri_agent::session::subagent::SubagentRunMode::Sync,
            llm,
            build_result
                .tools
                .into_iter()
                .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
                .collect(),
            build_result.system_prompt,
            build_result.skill_names,
            cwd,
        );

        let spawned = self.spawn(config).await?;

        // Interrupted 语义与迁移前一致
        if spawned.interrupted {
            return Ok("Sub-agent execution was interrupted".to_string());
        }

        if host.as_ref().and_then(|h| h.thread_store.clone()).is_some() {
            Ok(format!(
                "child_thread_id: {}
{}",
                spawned.child_thread_id,
                format_subagent_result(&peri_agent::agent::react::AgentOutput {
                    text: extract_last_ai_text(&spawned.session),
                    steps: 0,
                    tool_calls: Vec::new(),
                    stop_reason: None,
                    block_continue: None,
                })
            ))
        } else {
            Ok(format_subagent_result(
                &peri_agent::agent::react::AgentOutput {
                    text: extract_last_ai_text(&spawned.session),
                    steps: 0,
                    tool_calls: Vec::new(),
                    stop_reason: None,
                    block_continue: None,
                },
            ))
        }
    }
}

/// 复用 peri-agent 的 subagent 结果格式与文本提取
use peri_agent::session::subagent::{extract_last_ai_text, format_subagent_result};
