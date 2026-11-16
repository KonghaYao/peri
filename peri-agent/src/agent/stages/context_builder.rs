use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::events_v2::EventBus;
use crate::agent::react::ReactLLM;
use crate::agent::stages::context::StageContext;
use crate::agent::stages::context::StageContextBuilder;

use crate::agent::stages::SharedToolMap;
use crate::agent::token::ContextBudget;
use crate::middleware::chain::MiddlewareChain;
use crate::session::tool_catalog::SessionToolCatalog;
use crate::tools::ToolInvocationResolver;
use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

impl StageContextBuilder {
    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn ReactLLM + Send + Sync>) -> Self {
        self.runtime.llm = llm;
        self
    }

    #[must_use]
    pub fn with_tools(mut self, tools: SharedToolMap) -> Self {
        // `with_tools` is a builder convenience seam; production installs its
        // validated session catalog explicitly with `with_tool_catalog`.
        self.runtime.tool_catalog = Arc::new(
            SessionToolCatalog::try_new(tools.read().clone(), None)
                .unwrap_or_else(|_| SessionToolCatalog::new(BTreeMap::new(), None)),
        );
        self.runtime.tools = tools;
        self
    }

    #[must_use]
    pub fn with_tool_catalog(mut self, catalog: Arc<SessionToolCatalog>) -> Self {
        self.runtime.tool_catalog = catalog;
        self
    }

    #[must_use]
    pub fn with_tool_invocation_resolver(
        mut self,
        resolver: Arc<dyn ToolInvocationResolver>,
    ) -> Self {
        self.runtime.tool_invocation_resolver = resolver;
        self
    }

    #[must_use]
    pub fn with_middleware_chain(mut self, chain: Arc<MiddlewareChain>) -> Self {
        self.runtime.middleware_chain = chain;
        self
    }

    #[must_use]
    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.runtime.event_bus = bus;
        self
    }

    #[must_use]
    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.compact.context_budget = Some(budget);
        self
    }

    #[must_use]
    pub fn with_compact_config(mut self, config: CompactConfig) -> Self {
        self.compact.compact_config = Some(config);
        self
    }

    #[must_use]
    pub fn with_compact_llm(mut self, llm: Arc<dyn peri_model::Model>) -> Self {
        self.compact.compact_llm = Some(llm);
        self
    }

    #[must_use]
    pub fn with_shared_tools(mut self, shared: SharedToolMap) -> Self {
        self.runtime.shared_tools = Some(shared);
        self
    }

    #[must_use]
    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.session.agent_id = agent_id;
        self
    }

    #[must_use]
    pub fn with_async_result_mailbox(
        mut self,
        mailbox: Arc<crate::session::async_result_mailbox::AsyncResultMailbox>,
    ) -> Self {
        self.session.async_result_mailbox = mailbox;
        self
    }

    #[must_use]
    pub fn with_compact_exposure(
        mut self,
        exposure: Arc<parking_lot::Mutex<crate::agent::compact_v2::exposure::ExposureLedger>>,
    ) -> Self {
        self.compact.exposure = exposure;
        self
    }

    #[must_use]
    pub fn with_session_context(mut self, ctx: Arc<RwLock<HashMap<String, String>>>) -> Self {
        self.session.session_context = ctx;
        self
    }

    #[must_use]
    pub fn with_compact_pre_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.compact.compact_pre_hook = Some(hook);
        self
    }

    #[must_use]
    pub fn with_compact_post_hook(mut self, hook: Arc<dyn Fn(bool, usize) + Send + Sync>) -> Self {
        self.compact.compact_post_hook = Some(hook);
        self
    }

    #[must_use]
    pub fn with_goal_controller(
        mut self,
        controller: Arc<dyn peri_acp_types::goal::GoalController>,
    ) -> Self {
        self.goal_controller = Some(controller);
        self
    }

    #[must_use]
    pub fn build(self) -> StageContext {
        StageContext {
            session: self.session,
            runtime: self.runtime,
            compact: self.compact,
            async_ctx: self.async_ctx,
            goal_controller: self.goal_controller,
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
            model_view: Arc::new(crate::agent::stages::model_view::ModelViewScope::default()),
        }
    }
}
