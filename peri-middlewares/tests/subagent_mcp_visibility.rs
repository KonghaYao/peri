use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::session::subagent::SubagentLlmSource;
use peri_agent::{
    agent::{
        react::{ReactLLM, Reasoning, StreamingContext, ToolCall},
        stages::SharedToolMap,
    },
    messages::BaseMessage,
    middleware::{capabilities::CatalogState, r#trait::Middleware},
    session::{
        subagent::{build_v2_subagent_context, SubagentChainAssembler, SubagentChainContext},
        tool_catalog::{ToolFilterPolicy, ToolSource},
        FrozenContext, Session,
    },
    tools::{BaseTool, ToolContext},
};
use peri_middlewares::subagent::{SubAgentMiddleware, SubagentChainAssemblerImpl};
use tokio_util::sync::CancellationToken;

struct LookupTool;

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

struct LookupLLM(std::sync::atomic::AtomicUsize, bool);

#[async_trait]
impl ReactLLM for LookupLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
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
            return Ok(Reasoning::with_answer(
                "",
                "verified dynamic tool revocation",
            ));
        }
        match step {
            0 => Ok(Reasoning::with_tools(
                "discover",
                vec![ToolCall::new(
                    "search",
                    "SearchExtraTools",
                    serde_json::json!({"query": "select:mcp__late__lookup"}),
                )],
            )),
            1 => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("mcp__late__lookup"));
                Ok(Reasoning::with_tools(
                    "execute",
                    vec![ToolCall::new(
                        "execute",
                        "ExecuteExtraTool",
                        serde_json::json!({"tool_name": "mcp__late__lookup", "params": {}}),
                    )],
                ))
            }
            _ => {
                assert!(messages
                    .last()
                    .unwrap()
                    .content()
                    .contains("late capability executed"));
                Ok(Reasoning::with_answer(
                    "",
                    "verified deferred MCP execution",
                ))
            }
        }
    }
}

struct CatalogProbe(SharedToolMap, Option<ToolSource>);

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
async fn fork_inherits_late_static_mcp_tools_and_discovers_then_executes_them() {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let middleware = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_| {
            SubagentLlmSource::prebuilt(Box::new(LookupLLM(
                std::sync::atomic::AtomicUsize::new(0),
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
    assert!(
        result.contains("verified deferred MCP execution"),
        "{result}"
    );
}

fn filtered_child(
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
    build_v2_subagent_context(
        None,
        Box::new(LookupLLM(std::sync::atomic::AtomicUsize::new(0), true)),
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
fn explicit_zero_tools_does_not_gain_search_or_execution_tools() {
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
fn denied_mcp_tools_are_absent_from_the_child_search_and_execution_catalog() {
    let child = filtered_child(None, vec!["mcp__late__lookup".to_string()]);
    let snapshot = child.context.runtime.tool_catalog.snapshot();
    assert!(snapshot.tools.contains_key("SearchExtraTools"));
    assert!(snapshot.tools.contains_key("ExecuteExtraTool"));
    assert!(!snapshot.tools.contains_key("mcp__late__lookup"));
}

#[tokio::test]
async fn unloaded_dynamic_tools_are_not_frozen_into_static_child_inheritance() {
    use peri_acp_types::dynamic_mcp::{
        DynamicMcpIncarnationId, DynamicMcpInstanceKey, DynamicMcpLogicalKey,
    };

    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let middleware = SubAgentMiddleware::new(
        vec![],
        None,
        Arc::new(|_| {
            SubagentLlmSource::prebuilt(Box::new(LookupLLM(
                std::sync::atomic::AtomicUsize::new(0),
                false,
            )))
        }),
    );
    middleware.set_parent_session(parent);
    let tools = middleware
        .collect_tools("/tmp")
        .into_iter()
        .map(|tool| (tool.name().to_string(), Arc::from(tool)))
        .collect();
    let source = ToolSource::DynamicMcp(DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "parent".to_string(),
            server_name: "late".to_string(),
        },
        incarnation_id: DynamicMcpIncarnationId::from_string("revoked".to_string()),
    });
    let mut probe = CatalogProbe(Arc::new(RwLock::new(tools)), Some(source));
    probe
        .0
        .write()
        .insert("mcp__late__lookup".to_string(), Arc::new(LookupTool));
    middleware.before_reason_catalog(&mut probe).await.unwrap();
    let agent = probe.0.read()["Agent"].clone();
    let result = agent
        .invoke(
            serde_json::json!({"fork": true, "prompt": "check revoked tools"}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    assert!(
        result.contains("verified dynamic tool revocation"),
        "{result}"
    );
}
