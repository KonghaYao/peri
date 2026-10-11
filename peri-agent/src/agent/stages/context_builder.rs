use super::*;

impl StageContextBuilder {
    pub fn with_llm(mut self, llm: Arc<dyn ReactLLM + Send + Sync>) -> Self {
        self.runtime.llm = llm;
        self
    }

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

    pub fn with_tool_catalog(mut self, catalog: Arc<SessionToolCatalog>) -> Self {
        self.runtime.tool_catalog = catalog;
        self
    }

    pub fn with_tool_invocation_resolver(
        mut self,
        resolver: Arc<dyn ToolInvocationResolver>,
    ) -> Self {
        self.runtime.tool_invocation_resolver = resolver;
        self
    }

    pub fn with_middleware_chain(mut self, chain: Arc<MiddlewareChain>) -> Self {
        self.runtime.middleware_chain = chain;
        self
    }

    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.runtime.event_bus = bus;
        self
    }

    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.compact.context_budget = Some(budget);
        self
    }

    pub fn with_compact_config(mut self, config: CompactConfig) -> Self {
        self.compact.compact_config = Some(config);
        self
    }

    pub fn with_compact_llm(mut self, llm: Arc<dyn peri_model::Model>) -> Self {
        self.compact.compact_llm = Some(llm);
        self
    }

    pub fn with_shared_tools(mut self, shared: SharedToolMap) -> Self {
        self.runtime.shared_tools = Some(shared);
        self
    }

    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.session.agent_id = agent_id;
        self
    }

    pub fn with_session_context(mut self, ctx: Arc<RwLock<HashMap<String, String>>>) -> Self {
        self.session.session_context = ctx;
        self
    }

    pub fn with_compact_pre_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.compact.compact_pre_hook = Some(hook);
        self
    }

    pub fn with_compact_post_hook(mut self, hook: Arc<dyn Fn(bool, usize) + Send + Sync>) -> Self {
        self.compact.compact_post_hook = Some(hook);
        self
    }

    pub fn with_goal_controller(
        mut self,
        controller: Arc<dyn peri_acp_types::goal::GoalController>,
    ) -> Self {
        self.goal_controller = Some(controller);
        self
    }

    pub fn build(self) -> StageContext {
        StageContext {
            session: self.session,
            runtime: self.runtime,
            compact: self.compact,
            async_ctx: self.async_ctx,
            goal_controller: self.goal_controller,
            recall_buffer: Arc::new(RwLock::new(Vec::new())),
        }
    }
}
