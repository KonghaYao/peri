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
    messages::BaseMessage,
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
        false
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.result.to_string())
    }
}

#[derive(Clone)]
struct CatalogLLM {
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

/// 当前可见工具面 = 请求里的直接工具名 + 历史中**真实 Act** 执行 SearchExtraTools
/// 的结果（模型无法直接看到 deferred 工具，只能经真实搜索发现）。
fn visible_names(request: &peri_model::ModelRequest) -> Vec<String> {
    use crate::subagent::test_support::*;
    let defined = defined_tools(request);
    let mut names = defined
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&"mcp__dynamic__lookup".to_string()),
        "deferred 工具不得直接出现在请求工具面: {names:?}"
    );
    // 只看**最近一条** Tool 结果，并按真实 SearchExtraTools 的 JSON 输出判定：
    // 命中项出现在 `results` 里（查询串自身也会回显，不能用子串判定）。
    let latest = base_messages(request)
        .into_iter()
        .rev()
        .find_map(|message| match message {
            BaseMessage::Tool { content, .. } => Some(content.text_content()),
            _ => None,
        });
    if latest
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("results").cloned())
        .and_then(|results| results.as_array().cloned())
        .is_some_and(|results| {
            results.iter().any(|entry| {
                entry
                    .get("name")
                    .and_then(|name| name.as_str())
                    .is_some_and(|name| name == "mcp__dynamic__lookup")
            })
        })
    {
        names.push("mcp__dynamic__lookup".to_string());
    }
    names
}

/// 是否已有真实的 SearchExtraTools 结果（决定下一步是发起搜索还是读取结果）。
fn has_search_result(request: &peri_model::ModelRequest) -> bool {
    use crate::subagent::test_support::*;
    // 这些夹具唯一发出的工具调用就是 SearchExtraTools：历史里出现 Tool 结果即
    // 表示真实 Act 已完成一次搜索（结果文案不保证包含工具名）。
    base_messages(request)
        .iter()
        .any(|message| matches!(message, BaseMessage::Tool { .. }))
}

fn search_events(id: &str) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
    use crate::subagent::test_support::*;
    tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
        id,
        "SearchExtraTools",
        serde_json::json!({"query": "select:mcp__dynamic__lookup"}),
    )])
}

impl CatalogLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support::*;
        let _ = &cancellation;
        if has_search_result(&request) {
            // 真实 Act 已执行搜索：记录本代可见面后收尾（每个子会话一条记录）。
            self.seen.lock().unwrap().push(visible_names(&request));
            text_events("done")
        } else {
            search_events("catalog-search")
        }
    }
}
crate::subagent::test_support::fixture_model_impl!(CatalogLLM);

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

async fn production_tool(
    capability: Arc<MutableCapability>,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
) -> (SubAgentTool, HostFixture) {
    let host = HostFixture::open("fixture-dynamic-tool").await;
    let host = durable_host_with_capability(host, capability);
    let tool = host.bind(SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        Arc::new(move |_| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(CatalogLLM {
                    seen: Arc::clone(&seen),
                }),
                "fixture-scripted",
            )
        }),
        host.cwd.clone(),
    ));
    (tool, host)
}

/// 把会话级 MCP capability 发布面装到父 host（生产子链从父 host 读取）。
fn durable_host_with_capability(
    host: HostFixture,
    capability: Arc<MutableCapability>,
) -> HostFixture {
    let mut sub_host = host.parent_session_host().unwrap_or_default();
    sub_host.session_mcp_capability =
        Some(Arc::clone(&capability) as Arc<dyn SessionMcpCapabilityPort>);
    host.with_rebuilt_host(sub_host)
}

async fn invoke_fork(tool: &SubAgentTool, host: &HostFixture) {
    // 每次委派使用各自的受信 invocation（一个 invocation 只绑定一个任务）。
    let invocation = host.fresh_tool_call_id("fork");
    tool.invoke(
        serde_json::json!({"fork": true, "prompt": "inspect"}),
        host.context_with(&[], invocation),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn existing_fork_child_refreshes_across_load_and_unload_reason_boundaries() {
    #[derive(Clone)]
    struct RefreshingLLM {
        capability: Arc<MutableCapability>,
        seen: Arc<Mutex<Vec<Vec<String>>>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RefreshingLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let _messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            let _ = &tools;
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // 目录在 **Reason 边界** 发布：本轮的 Act 搜索使用本轮 Reason 已发布的
            // 目录，因此「先改能力 + 下一轮搜索」才能观察到对应代际。
            match call {
                0 => {
                    self.seen.lock().unwrap().push(visible_names(&request));
                    *self.capability.0.write() =
                        Arc::new(capability_snapshot(1, Some("mcp__dynamic__lookup")));
                    tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
                        "refresh-read-0",
                        "Read",
                        serde_json::json!({}),
                    )])
                }
                1 => search_events("refresh-search-1"),
                2 => {
                    self.seen.lock().unwrap().push(visible_names(&request));
                    *self.capability.0.write() = Arc::new(capability_snapshot(2, None));
                    search_events("refresh-search-2")
                }
                // 目录在下一轮 Reason 才发布 gen2：再搜索一次才能观察到“下架”。
                3 => search_events("refresh-search-3"),
                _ => {
                    self.seen.lock().unwrap().push(visible_names(&request));
                    text_events("done")
                }
            }
        }
    }
    crate::subagent::test_support::fixture_model_impl!(RefreshingLLM);

    let capability = Arc::new(MutableCapability::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let host = HostFixture::open("fixture-dynamic-refresh").await;
    let host = durable_host_with_capability(host, Arc::clone(&capability));
    let tool = host.bind(SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        {
            let capability = Arc::clone(&capability);
            let seen = Arc::clone(&seen);
            Arc::new(move |_| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(RefreshingLLM {
                        capability: Arc::clone(&capability),
                        seen: Arc::clone(&seen),
                        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    }),
                    "fixture-scripted",
                )
            })
        },
        host.cwd.clone(),
    ));

    invoke_fork(&tool, &host).await;

    let seen = seen.lock().unwrap();
    assert!(
        !seen[0].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
    assert!(
        seen[1].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
    assert!(
        !seen[2].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
}

#[tokio::test]
async fn generation_n_dispatch_stays_pinned_after_n_plus_one_is_published() {
    #[derive(Clone)]
    struct PinningLLM {
        capability: Arc<MutableCapability>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        observed_result: Arc<Mutex<Option<String>>>,
    }

    impl PinningLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let _tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                *self.capability.0.write() = Arc::new(capability_snapshot_with_result(
                    2,
                    Some("mcp__dynamic__lookup"),
                    "generation-n-plus-one",
                ));
                return tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
                    "call-1",
                    "ExecuteExtraTool",
                    serde_json::json!({"tool_name": "mcp__dynamic__lookup", "params": {}}),
                )]);
            }
            *self.observed_result.lock().unwrap() =
                messages.last().map(|message| message.content());
            text_events("done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(PinningLLM);

    let capability = Arc::new(MutableCapability(RwLock::new(Arc::new(
        capability_snapshot_with_result(1, Some("mcp__dynamic__lookup"), "generation-n"),
    ))));
    let observed_result = Arc::new(Mutex::new(None));
    let host = HostFixture::open("fixture-dynamic-pinning").await;
    let host = durable_host_with_capability(host, Arc::clone(&capability));
    let tool = host.bind(SubAgentTool::new(
        Arc::new(vec![make_tool("Read")]),
        None,
        {
            let capability = Arc::clone(&capability);
            let observed_result = Arc::clone(&observed_result);
            Arc::new(move |_| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(PinningLLM {
                        capability: Arc::clone(&capability),
                        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                        observed_result: Arc::clone(&observed_result),
                    }),
                    "fixture-scripted",
                )
            })
        },
        host.cwd.clone(),
    ));

    invoke_fork(&tool, &host).await;

    assert_eq!(
        observed_result.lock().unwrap().as_deref(),
        Some("generation-n")
    );
}

#[tokio::test]
async fn existing_and_new_fork_children_refresh_the_parent_session_publisher() {
    let capability = Arc::new(MutableCapability::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (tool, host) = production_tool(Arc::clone(&capability), Arc::clone(&seen)).await;

    invoke_fork(&tool, &host).await;
    *capability.0.write() = Arc::new(capability_snapshot(1, Some("mcp__dynamic__lookup")));
    invoke_fork(&tool, &host).await;
    *capability.0.write() = Arc::new(capability_snapshot(2, None));
    invoke_fork(&tool, &host).await;

    let seen = seen.lock().unwrap();
    assert!(
        !seen[0].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
    assert!(
        seen[1].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
    assert!(
        !seen[2].contains(&"mcp__dynamic__lookup".to_string()),
        "{seen:?}"
    );
}
