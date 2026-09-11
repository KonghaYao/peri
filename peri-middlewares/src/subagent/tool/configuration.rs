//! Public builders and parent-host fallback access for the single tool owner.
use super::SubagentChainAssemblerImpl;
use crate::{agent_define::AgentOverrides, hooks::types::RegisteredHook, mcp::McpAgentRegistry};
use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::session::subagent::SubagentHost;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    messages::BaseMessage,
    tools::BaseTool,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

impl super::SubAgentTool {
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
            plugin_agent_dirs: Arc::new(Vec::new()),
            mcp_agent_registry: None,
            broker: None,
            chain_assembler: Arc::new(SubagentChainAssemblerImpl),
        }
    }

    pub(crate) fn with_plugin_agent_dirs(mut self, dirs: Arc<Vec<std::path::PathBuf>>) -> Self {
        self.plugin_agent_dirs = dirs;
        self
    }

    pub(crate) fn with_mcp_agents(
        mut self,
        registry: Option<Arc<McpAgentRegistry>>,
        broker: Option<Arc<dyn peri_agent::interaction::UserInteractionBroker>>,
    ) -> Self {
        self.mcp_agent_registry = registry;
        self.broker = broker;
        self
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
    /// 生产路径 frozen 数据由 [`SessionFactory::spawn_subagent`](peri_agent::session::subagent::SessionFactory::spawn_subagent) 从 parent session copy）。
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
    pub(crate) fn host(&self) -> Arc<SubagentHost> {
        self.parent_session
            .read()
            .as_ref()
            .and_then(|s| s.subagent_host())
            .unwrap_or_else(|| Arc::new(self.host.clone()))
    }
}
