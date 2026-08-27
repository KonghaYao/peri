use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpConfig, CanonicalDynamicMcpTransport, DynamicMcpIncarnationId,
        DynamicMcpInstanceKey, DynamicMcpLogicalKey, DynamicMcpServerProjection,
        DynamicMcpToolCapability, SessionMcpCapabilitySnapshot,
    },
    ports::SessionMcpCapabilityPort,
};
use peri_agent::{
    agent::react::{ReactLLM, Reasoning, StreamingContext},
    messages::BaseMessage,
    session::{subagent::SubagentHost, FrozenContext, Session},
    tools::{BaseTool, ToolContext},
};

use super::*;

#[derive(Default)]
struct MutableCapability(RwLock<Arc<SessionMcpCapabilitySnapshot>>);

impl SessionMcpCapabilityPort for MutableCapability {
    fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
        Arc::clone(&self.0.read())
    }
}

struct NamedTool {
    name: &'static str,
    result: &'static str,
}

#[async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "dynamic test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn is_direct(&self) -> bool {
        true
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.result.to_string())
    }
}

struct CatalogLLM {
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ReactLLM for CatalogLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        self.seen
            .lock()
            .unwrap()
            .push(tools.iter().map(|tool| tool.name().to_string()).collect());
        Ok(Reasoning::with_answer("", "done"))
    }
}

fn capability_snapshot_with_result(
    generation: u64,
    tool: Option<&'static str>,
    result: &'static str,
) -> SessionMcpCapabilitySnapshot {
    let Some(tool_name) = tool else {
        return SessionMcpCapabilitySnapshot {
            generation,
            ..Default::default()
        };
    };
    let instance = DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "session-a".to_string(),
            server_name: "dynamic".to_string(),
        },
        incarnation_id: DynamicMcpIncarnationId::from_string(format!("inc-{generation}")),
    };
    let config = CanonicalDynamicMcpConfig {
        transport: CanonicalDynamicMcpTransport::Stdio {
            command: "test".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout_ms: 1,
        protocol_version: None,
        subscriptions: None,
    };
    SessionMcpCapabilitySnapshot {
        generation,
        servers: BTreeMap::from([(
            "dynamic".to_string(),
            DynamicMcpServerProjection {
                instance_key: instance.clone(),
                name: "dynamic".to_string(),
                config,
                tool_count: 1,
                resource_count: 0,
            },
        )]),
        tools: BTreeMap::from([(
            tool_name.to_string(),
            DynamicMcpToolCapability {
                instance,
                tool: Arc::new(NamedTool {
                    name: tool_name,
                    result,
                }),
            },
        )]),
    }
}

fn capability_snapshot(
    generation: u64,
    tool: Option<&'static str>,
) -> SessionMcpCapabilitySnapshot {
    capability_snapshot_with_result(generation, tool, tool.unwrap_or("unloaded"))
}

fn production_tool(
    capability: Arc<MutableCapability>,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
) -> SubAgentTool {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    parent.set_subagent_host(SubagentHost {
        session_mcp_capability: Some(capability),
        ..Default::default()
    });
    SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        Arc::new(move |_| {
            Box::new(CatalogLLM {
                seen: Arc::clone(&seen),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_session(parent)
}

async fn invoke_fork(tool: &SubAgentTool) {
    tool.invoke(
        serde_json::json!({"fork": true, "prompt": "inspect"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn existing_fork_child_refreshes_across_load_and_unload_reason_boundaries() {
    struct RefreshingLLM {
        capability: Arc<MutableCapability>,
        seen: Arc<Mutex<Vec<Vec<String>>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ReactLLM for RefreshingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.seen
                .lock()
                .unwrap()
                .push(tools.iter().map(|tool| tool.name().to_string()).collect());
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match call {
                0 => {
                    *self.capability.0.write() =
                        Arc::new(capability_snapshot(1, Some("mcp__dynamic__lookup")));
                }
                1 => {
                    *self.capability.0.write() = Arc::new(capability_snapshot(2, None));
                }
                _ => return Ok(Reasoning::with_answer("", "done")),
            }
            Ok(Reasoning::with_tools(
                "continue",
                vec![peri_agent::agent::react::ToolCall::new(
                    format!("call-{call}"),
                    "Read",
                    serde_json::json!({}),
                )],
            ))
        }
    }

    let capability = Arc::new(MutableCapability::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    parent.set_subagent_host(SubagentHost {
        session_mcp_capability: Some(Arc::clone(&capability) as Arc<dyn SessionMcpCapabilityPort>),
        ..Default::default()
    });
    let tool = SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        {
            let capability = Arc::clone(&capability);
            let seen = Arc::clone(&seen);
            Arc::new(move |_| {
                Box::new(RefreshingLLM {
                    capability: Arc::clone(&capability),
                    seen: Arc::clone(&seen),
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }) as Box<dyn ReactLLM + Send + Sync>
            })
        },
        "/tmp".to_string(),
    )
    .with_parent_session(parent);

    invoke_fork(&tool).await;

    let seen = seen.lock().unwrap();
    assert!(!seen[0].contains(&"mcp__dynamic__lookup".to_string()));
    assert!(seen[1].contains(&"mcp__dynamic__lookup".to_string()));
    assert!(!seen[2].contains(&"mcp__dynamic__lookup".to_string()));
}

#[tokio::test]
async fn generation_n_dispatch_stays_pinned_after_n_plus_one_is_published() {
    struct PinningLLM {
        capability: Arc<MutableCapability>,
        calls: std::sync::atomic::AtomicUsize,
        observed_result: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl ReactLLM for PinningLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                *self.capability.0.write() = Arc::new(capability_snapshot_with_result(
                    2,
                    Some("mcp__dynamic__lookup"),
                    "generation-n-plus-one",
                ));
                return Ok(Reasoning::with_tools(
                    "use pinned tool",
                    vec![peri_agent::agent::react::ToolCall::new(
                        "call-1",
                        "mcp__dynamic__lookup",
                        serde_json::json!({}),
                    )],
                ));
            }
            *self.observed_result.lock().unwrap() =
                messages.last().map(|message| message.content());
            Ok(Reasoning::with_answer("", "done"))
        }
    }

    let capability = Arc::new(MutableCapability(RwLock::new(Arc::new(
        capability_snapshot_with_result(1, Some("mcp__dynamic__lookup"), "generation-n"),
    ))));
    let observed_result = Arc::new(Mutex::new(None));
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    parent.set_subagent_host(SubagentHost {
        session_mcp_capability: Some(Arc::clone(&capability) as Arc<dyn SessionMcpCapabilityPort>),
        ..Default::default()
    });
    let tool = SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        {
            let capability = Arc::clone(&capability);
            let observed_result = Arc::clone(&observed_result);
            Arc::new(move |_| {
                Box::new(PinningLLM {
                    capability: Arc::clone(&capability),
                    calls: std::sync::atomic::AtomicUsize::new(0),
                    observed_result: Arc::clone(&observed_result),
                }) as Box<dyn ReactLLM + Send + Sync>
            })
        },
        "/tmp".to_string(),
    )
    .with_parent_session(parent);

    invoke_fork(&tool).await;

    assert_eq!(
        observed_result.lock().unwrap().as_deref(),
        Some("generation-n")
    );
}

#[tokio::test]
async fn existing_and_new_fork_children_refresh_the_parent_session_publisher() {
    let capability = Arc::new(MutableCapability::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tool = production_tool(Arc::clone(&capability), Arc::clone(&seen));

    invoke_fork(&tool).await;
    *capability.0.write() = Arc::new(capability_snapshot(1, Some("mcp__dynamic__lookup")));
    invoke_fork(&tool).await;
    *capability.0.write() = Arc::new(capability_snapshot(2, None));
    invoke_fork(&tool).await;

    let seen = seen.lock().unwrap();
    assert!(!seen[0].contains(&"mcp__dynamic__lookup".to_string()));
    assert!(seen[1].contains(&"mcp__dynamic__lookup".to_string()));
    assert!(!seen[2].contains(&"mcp__dynamic__lookup".to_string()));
}
