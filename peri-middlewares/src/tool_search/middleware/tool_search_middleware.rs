use super::{
    async_trait, declaration, fmt, hook_state, AgentResult, Arc, BTreeMap, BaseTool, Debug,
    ExecuteExtraTool, Formatter, Middleware, RwLock, SearchExtraTools, StdRwLock, ToolSearchIndex,
};

/// ToolSearch 中间件
///
/// 职责：
/// 1. 注册 SearchExtraTools 和 ExecuteExtraTool 两个元工具
/// 2. 在 before_agent 时注入延迟工具列表到 system prompt
pub struct ToolSearchMiddleware {
    pub(super) tool_search_index: Arc<ToolSearchIndex>,
    pub(super) shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    pub(super) cached_contribution: Arc<StdRwLock<Option<String>>>,
    /// 当前索引绑定的 deferred snapshot 指纹。
    pub(super) deferred_fingerprint: Arc<StdRwLock<Option<String>>>,
}

impl Debug for ToolSearchMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolSearchMiddleware")
            .finish_non_exhaustive()
    }
}

impl ToolSearchMiddleware {
    #[must_use]
    pub fn new(
        tool_search_index: Arc<ToolSearchIndex>,
        shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    ) -> Self {
        Self {
            tool_search_index,
            shared_tools,
            cached_contribution: Arc::new(StdRwLock::new(None)),
            deferred_fingerprint: Arc::new(StdRwLock::new(None)),
        }
    }

    pub(super) fn deferred_fingerprint(tools: &[Arc<dyn BaseTool>]) -> String {
        serde_json::to_string(
            &tools
                .iter()
                .map(|tool| (tool.name(), tool.description(), tool.parameters()))
                .collect::<Vec<_>>(),
        )
        .expect("tool snapshot fingerprint must serialize")
    }

    pub(super) async fn rebind_catalog(
        &self,
        state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        // 优先读取 v2 每 turn 本地工具视图（stage_builder 构建，含当前链全部
        // 工具）；无本地视图时回退宿主级 shared_tools（v1 / 测试路径）。
        //
        // H5 可见性否决：`visible_to_model()` 是所有模型面投影的统一前置条件，
        // `is_direct()` 只区分 direct/deferred。app-only（`model:false`）工具
        // 保留在底层注册表（App 合法调用路径不变），但不得进入索引、deferred
        // 列表、direct 声明段与元工具描述。
        let deferred_arcs: Vec<Arc<dyn BaseTool>>;
        let direct_arcs: Vec<Arc<dyn BaseTool>>;
        let request_index = Arc::new(ToolSearchIndex::new());
        {
            let local = state.local_tools();
            let mut guard = match local {
                Some(tools) => tools.write(),
                None => self.shared_tools.write(),
            };
            let direct_names: Vec<String> = guard
                .iter()
                .filter(|(_, tool)| tool.is_direct() && tool.visible_to_model())
                .map(|(name, _)| name.clone())
                .collect();
            deferred_arcs = guard
                .iter()
                .filter(|(_, tool)| !tool.is_direct() && tool.visible_to_model())
                .map(|(_, tool)| Arc::clone(tool))
                .collect();
            request_index.build(deferred_arcs.clone());
            if guard
                .get(super::super::core_tools::SEARCH_EXTRA_TOOLS_NAME)
                .is_some_and(|tool| {
                    tool.mcp_server_name().is_none() && tool.namespace() == Some("meta")
                })
            {
                guard.insert(
                    super::super::core_tools::SEARCH_EXTRA_TOOLS_NAME.to_string(),
                    Arc::new(SearchExtraTools::with_direct_tools(
                        Arc::clone(&request_index),
                        direct_names.iter().map(String::as_str),
                    )),
                );
            }
            let request_resolver = Arc::new(RwLock::new(guard.clone()));
            if guard
                .get(super::super::core_tools::EXECUTE_EXTRA_TOOL_NAME)
                .is_some_and(|tool| {
                    tool.mcp_server_name().is_none() && tool.namespace() == Some("meta")
                })
            {
                guard.insert(
                    super::super::core_tools::EXECUTE_EXTRA_TOOL_NAME.to_string(),
                    Arc::new(ExecuteExtraTool::with_direct_tools(
                        request_resolver,
                        direct_names.iter().map(String::as_str),
                    )),
                );
            }
            direct_arcs = guard
                .iter()
                .filter(|(_, tool)| tool.is_direct() && tool.visible_to_model())
                .map(|(_, tool)| Arc::clone(tool))
                .collect();
        }

        let fingerprint = Self::deferred_fingerprint(&deferred_arcs);
        let should_rebuild = self
            .deferred_fingerprint
            .read()
            .expect("deferred tool catalog fingerprint must remain consistent with its cache")
            .as_ref()
            != Some(&fingerprint);
        if should_rebuild {
            let old_count = self.tool_search_index.total_count();
            self.tool_search_index.build(deferred_arcs);
            *self.deferred_fingerprint.write().expect(
                "deferred tool catalog fingerprint must remain consistent with its cache",
            ) = Some(fingerprint);
            let new_count = self.tool_search_index.total_count();
            if old_count > 0 && new_count != old_count {
                state.push_recall(format!(
                    "[ToolSearch] Deferred tools updated: {} tools available (was {})",
                    new_count, old_count
                ));
            }
        }

        let list = request_index.format_deferred_list();
        let list = (!list.is_empty()).then_some(list);
        let declarations = declaration::collect_declarations(&direct_arcs);
        *self
            .cached_contribution
            .write()
            .expect("frozen prompt contribution must not expose an interrupted cache update") =
            match (list, declarations) {
                (Some(l), Some(d)) => Some(format!("{l}\n\n{d}")),
                (Some(l), None) => Some(l),
                (None, Some(d)) => Some(d),
                (None, None) => None,
            };
        Ok(())
    }
}

#[async_trait]
impl Middleware for ToolSearchMiddleware {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![
            Box::new(SearchExtraTools::new(Arc::clone(&self.tool_search_index))),
            Box::new(ExecuteExtraTool::new(Arc::clone(&self.shared_tools))),
        ]
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution
            .read()
            .expect("frozen prompt contribution must not expose an interrupted cache update")
            .clone()
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        self.rebind_catalog(state).await
    }

    async fn before_reason_catalog(
        &self,
        state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        self.rebind_catalog(state).await
    }
}
