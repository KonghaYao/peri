use super::{
    async_trait, inline_fixture, subagent, Arc, BTreeMap, BaseTool, CancellationToken,
    CatalogState, FrozenContext, RwLock, Session, SharedToolMap, SubAgentMiddleware,
    SubagentChainAssemblerImpl, SubagentChainContext, ToolCall, ToolContext, ToolFilterPolicy,
    ToolSource,
};
use peri_agent::middleware::Middleware as _;
use peri_agent::session::subagent::SubagentChainAssembler as _;

pub(super) struct LookupTool;

#[async_trait]
impl BaseTool for LookupTool {
    fn name(&self) -> &str {
        "mcp__late__lookup"
    }
    fn description(&self) -> &str {
        "Lookup a late-connected capability"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn is_direct(&self) -> bool {
        false
    }
    fn mcp_server_name(&self) -> Option<&str> {
        Some("late")
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        Some("lookup")
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("late capability executed".to_string())
    }
}

#[derive(Clone)]
pub(super) struct LookupLLM(
    pub(super) Arc<std::sync::atomic::AtomicUsize>,
    pub(super) bool,
);

impl LookupLLM {
    pub(super) async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::inline_fixture;
        let _ = &cancellation;
        let messages = inline_fixture::base_messages(&request);
        let defined = inline_fixture::defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        assert!(!tools.iter().any(|tool| tool.name() == "mcp__late__lookup"));
        assert!(tools.iter().any(|tool| tool.name() == "SearchExtraTools"));
        assert!(tools.iter().any(|tool| tool.name() == "ExecuteExtraTool"));
        let step = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if step == 1 && !self.1 {
            assert!(!messages
                .last()
                .unwrap()
                .content()
                .contains("mcp__late__lookup"));
            return inline_fixture::text_events("verified dynamic tool revocation");
        }
        match step {
            0 => inline_fixture::tool_events_from_react(vec![ToolCall::new(
                "search",
                "SearchExtraTools",
                serde_json::json!({"query": "select:mcp__late__lookup"}),
            )]),
            1 => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("mcp__late__lookup"));
                inline_fixture::tool_events_from_react(vec![ToolCall::new(
                    "execute",
                    "ExecuteExtraTool",
                    serde_json::json!({"tool_name": "mcp__late__lookup", "params": {}}),
                )])
            }
            _ => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("late capability executed"));
                inline_fixture::text_events("verified deferred MCP execution")
            }
        }
    }
}

inline_fixture::fixture_model_impl!(LookupLLM);

pub(super) struct CatalogProbe(pub(super) SharedToolMap, pub(super) Option<ToolSource>);

impl CatalogState for CatalogProbe {
    fn local_tools(&self) -> Option<&SharedToolMap> {
        Some(&self.0)
    }
    fn push_recall(&mut self, _item: String) {}
    fn tool_source(&self, name: &str) -> Option<ToolSource> {
        (name == "mcp__late__lookup")
            .then(|| self.1.clone())
            .flatten()
    }
}

#[tokio::test]
pub(super) async fn fork_inherits_late_static_mcp_tools_and_discovers_then_executes_them() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    // v5：Agent 调用恒后台，集成夹具必须提供任务管理器与完成回调；
    // 不提供同步回退，缺管理器时工具会显式报错。
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = Arc::new(std::sync::Mutex::new(Some(completed_tx)));
    parent.set_subagent_host(peri_agent::session::subagent::SubagentHost {
        task_manager: Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
        on_bg_complete: Some(Arc::new(move |result, _kind| {
            if let Some(sender) = completed_tx.lock().unwrap().take() {
                let _ = sender.send(result.clone());
            }
            Ok(())
        })),
        ..Default::default()
    });
    let middleware = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_| {
            inline_fixture::fixture_source(std::sync::Arc::new(LookupLLM(
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                true,
            )))
        }),
    );
    middleware.set_parent_session(parent);
    let tools: BTreeMap<String, Arc<dyn BaseTool>> = middleware
        .collect_tools("/tmp")
        .into_iter()
        .map(|tool| (tool.name().to_string(), Arc::from(tool)))
        .collect();
    let mut probe = CatalogProbe(Arc::new(RwLock::new(tools)), None);
    probe
        .0
        .write()
        .insert("mcp__late__lookup".to_string(), Arc::new(LookupTool));
    middleware.before_reason_catalog(&mut probe).await.unwrap();
    let agent = probe.0.read()["Agent"].clone();
    let result = agent
        .invoke(
            serde_json::json!({"fork": true, "prompt": "use the MCP lookup"}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    assert!(result.contains("Background task"), "{result}");
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), completed_rx)
        .await
        .expect("background completion")
        .expect("completion callback");
    // 集成夹具没有 session_resources：后台终态落库会追加
    // "session resources unavailable" 并标记 success=false。本用例只验证 fork
    // 后的动态 MCP 工具发现与执行，仍发生在后台子链内。
    assert!(
        completed.output.contains("verified deferred MCP execution"),
        "{:?}",
        completed.output
    );
}

pub(super) fn filtered_child(
    allowed: Option<Vec<String>>,
    disallowed: Vec<String>,
) -> peri_agent::session::subagent::V2SubagentContext {
    let chain = SubagentChainAssemblerImpl::new().assemble(&SubagentChainContext {
        cwd: "/tmp".to_string(),
        skill_names: vec![],
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        meta_harness_disabled: Default::default(),
    });
    subagent::build_v2_subagent_context(
        None,
        Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
            std::sync::Arc::new(LookupLLM(
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                true,
            )),
        )),
        Arc::new(chain),
        vec![Arc::new(LookupTool)],
        ToolFilterPolicy::canonical(allowed, disallowed),
        None,
        "/tmp",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        None,
    )
}

#[test]
pub(super) fn explicit_zero_tools_does_not_gain_search_or_execution_tools() {
    let child = filtered_child(Some(vec![]), vec![]);
    assert!(child
        .context
        .runtime
        .tool_catalog
        .snapshot()
        .tools
        .is_empty());
}

#[test]
pub(super) fn denied_mcp_tools_are_absent_from_the_child_search_and_execution_catalog() {
    let child = filtered_child(None, vec!["mcp__late__lookup".to_string()]);
    let snapshot = child.context.runtime.tool_catalog.snapshot();
    assert!(snapshot.tools.contains_key("SearchExtraTools"));
    assert!(snapshot.tools.contains_key("ExecuteExtraTool"));
    assert!(!snapshot.tools.contains_key("mcp__late__lookup"));
}
