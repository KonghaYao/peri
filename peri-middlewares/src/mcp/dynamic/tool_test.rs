use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpAction, DynamicMcpAccepted, DynamicMcpAction, DynamicMcpFailure,
        DynamicMcpOperationId, DynamicMcpOperationState, DynamicMcpResponse,
        DynamicMcpShutdownReport,
    },
    ports::DynamicMcpDeploymentPort,
};
use peri_agent::{
    agent::{react::ToolCall, state::AgentState},
    interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
    },
    tools::{BaseTool, ToolContext, ToolInvocationResolver},
};
use serde_json::json;

use super::*;
use crate::tool_search::{ExecuteExtraTool, ExecuteExtraToolResolver};

#[derive(Default)]
struct FakeDeployment {
    actions: Mutex<Vec<CanonicalDynamicMcpAction>>,
}

#[async_trait]
impl DynamicMcpDeploymentPort for FakeDeployment {
    async fn execute(
        &self,
        _session_id: &str,
        action: CanonicalDynamicMcpAction,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let server = match &action {
            CanonicalDynamicMcpAction::Load(value) => value.name.clone(),
            CanonicalDynamicMcpAction::Unload(value) => value.name.clone(),
            CanonicalDynamicMcpAction::Status(_) => "status".to_string(),
        };
        self.actions.lock().unwrap().push(action);
        Ok(DynamicMcpResponse::Accepted(DynamicMcpAccepted {
            operation_id: DynamicMcpOperationId::from_string("mcpop_test"),
            server,
            state: DynamicMcpOperationState::Starting,
            scope: "session".to_string(),
            idempotent: false,
        }))
    }

    fn register_catalog(
        &self,
        _session_id: &str,
        _tools: Vec<peri_acp_types::dynamic_mcp::DynamicMcpCatalogTool>,
    ) -> Result<(), DynamicMcpFailure> {
        Ok(())
    }

    fn capability(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionMcpCapabilityPort> {
        struct Empty;
        impl peri_acp_types::ports::SessionMcpCapabilityPort for Empty {
            fn snapshot(&self) -> Arc<peri_acp_types::dynamic_mcp::SessionMcpCapabilitySnapshot> {
                Arc::new(Default::default())
            }
        }
        Arc::new(Empty)
    }

    fn close_registration(
        &self,
        _session_id: &str,
    ) -> Arc<dyn peri_acp_types::ports::SessionCloseRegistration> {
        struct Noop;
        #[async_trait]
        impl peri_acp_types::ports::SessionCloseRegistration for Noop {
            async fn revoke_and_cleanup(&self) -> DynamicMcpShutdownReport {
                DynamicMcpShutdownReport::Complete
            }
        }
        Arc::new(Noop)
    }

    fn begin_shutdown(&self) {}

    async fn close_session(&self, _session_id: &str) -> DynamicMcpShutdownReport {
        DynamicMcpShutdownReport::Complete
    }

    async fn shutdown(&self) -> DynamicMcpShutdownReport {
        DynamicMcpShutdownReport::Complete
    }
}

fn load_input() -> serde_json::Value {
    json!({
        "method": "load",
        "params": {
            "name": "example",
            "config": {
                "command": "example-mcp",
                "args": ["stdio"],
                "env": {"TOKEN": {"secretRef": "example-token"}}
            }
        }
    })
}

#[test]
fn parameters_describe_method_specific_contracts() {
    let tool = DynamicMcpTool::new("session-a", Arc::new(FakeDeployment::default()));
    let schema = tool.parameters();
    let branches = schema["oneOf"].as_array().unwrap();

    assert_eq!(branches[0]["properties"]["method"]["enum"][0], "load");
    assert_eq!(branches[0]["required"], json!(["method", "params"]));
    assert_eq!(branches[1]["properties"]["method"]["enum"][0], "status");
    assert_eq!(branches[1]["required"], json!(["method"]));
    assert_eq!(branches[2]["properties"]["method"]["enum"][0], "unload");
    assert_eq!(branches[2]["required"], json!(["method", "params"]));
}

#[test]
fn load_schema_selects_stdio_or_http_and_exposes_timeout_ms() {
    let tool = DynamicMcpTool::new("session-a", Arc::new(FakeDeployment::default()));
    let schema = tool.parameters();
    let configs = &schema["oneOf"][0]["properties"]["params"]["properties"]["config"]["oneOf"];

    assert_eq!(configs[0]["required"], json!(["command"]));
    assert_eq!(configs[1]["required"], json!(["url"]));
    assert_eq!(configs[0]["properties"]["timeoutMs"]["type"], "integer");
    assert_eq!(configs[0]["properties"]["timeoutMs"]["minimum"], 1);
    assert_eq!(configs[1]["properties"]["timeoutMs"]["type"], "integer");
    assert_eq!(configs[1]["properties"]["timeoutMs"]["minimum"], 1);
}

#[tokio::test]
async fn execute_extra_tool_projects_method_level_policy_and_binds_action() {
    let deployment = Arc::new(FakeDeployment::default());
    let registry: Arc<parking_lot::RwLock<std::collections::BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(parking_lot::RwLock::new(Default::default()));
    let dynamic: Arc<dyn BaseTool> = Arc::new(DynamicMcpTool::new(
        "session-a",
        Arc::clone(&deployment) as Arc<dyn DynamicMcpDeploymentPort>,
    ));
    registry
        .write()
        .insert(DYNAMIC_MCP_TOOL_NAME.to_string(), Arc::clone(&dynamic));
    let wrapper: Arc<dyn BaseTool> = Arc::new(ExecuteExtraTool::new(Arc::clone(&registry)));
    let mut tools = registry.read().clone();
    tools.insert(wrapper.name().to_string(), wrapper);

    let invocation = ExecuteExtraToolResolver::default()
        .resolve(
            &ToolCall::new(
                "call-1",
                "ExecuteExtraTool",
                json!({"tool_name": DYNAMIC_MCP_TOOL_NAME, "params": load_input()}),
            ),
            &tools,
        )
        .unwrap();

    assert_eq!(invocation.policy_call.name, "DynamicMCP.load");
    assert_eq!(
        invocation.policy_call.input["config"]["env"]["TOKEN"],
        "example-token"
    );
    invocation
        .target
        .invoke(
            json!({"method": "status", "params": {}}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    assert!(matches!(
        deployment.actions.lock().unwrap().as_slice(),
        [CanonicalDynamicMcpAction::Load(_)]
    ));
}

#[test]
fn invalid_dynamic_input_fails_before_deployment_or_hitl() {
    let deployment = Arc::new(FakeDeployment::default());
    let tool = DynamicMcpTool::new(
        "session-a",
        Arc::clone(&deployment) as Arc<dyn DynamicMcpDeploymentPort>,
    );
    let result = tool.bind_invocation(json!({
        "method": "load",
        "params": {
            "name": "a.b",
            "config": {"command": "example"}
        }
    }));
    assert!(result.is_err());
    assert!(deployment.actions.lock().unwrap().is_empty());
}

/// 边界回归：动态 MCP 路径继续拒绝 System key，不实现 session 中途声明语义。
///
/// 三个 canonical key 与三个 camelCase alias 都必须在 wire 反序列化与 canonical bind
/// 之前失败，不得降级为可执行请求，也不得触达 deployment load。
#[test]
fn test_dynamic_mcp_rejects_system_mcp_fields() {
    let deployment = Arc::new(FakeDeployment::default());
    let tool = DynamicMcpTool::new(
        "session-a",
        Arc::clone(&deployment) as Arc<dyn DynamicMcpDeploymentPort>,
    );

    let cases = [
        ("system_mcp", json!(true)),
        ("systemMcp", json!(true)),
        ("system_mcp_tools", json!(["Read"])),
        ("systemMcpTools", json!(["Read"])),
        ("system_mcp_timeout", json!(1500)),
        ("systemMcpTimeout", json!(1500)),
    ];

    // 对照：不含 System key 的同一配置可以正常绑定，拒绝确实由 System key 引起。
    let bound = tool
        .bind_invocation(json!({
            "method": "load",
            "params": {
                "name": "example",
                "config": {"command": "example-mcp", "args": ["stdio"]}
            }
        }))
        .expect("不含 System key 的配置必须可绑定")
        .expect("load 必须返回可执行绑定");
    assert_eq!(bound.policy_name, "DynamicMCP.load");

    for (key, value) in cases {
        let mut config = json!({"command": "example-mcp", "args": ["stdio"]});
        config[key] = value;
        let input = json!({
            "method": "load",
            "params": {"name": "example", "config": config}
        });

        assert!(
            DynamicMcpAction::from_tool_input(input.clone()).is_err(),
            "{key} 必须在 wire 反序列化阶段被拒绝"
        );
        assert!(
            tool.bind_invocation(input).is_err(),
            "{key} 必须在 canonical bind 前被拒绝，而不是降级为可执行请求"
        );
        assert!(
            deployment.actions.lock().unwrap().is_empty(),
            "{key} 被拒绝时不得调用 deployment load"
        );
    }

    // 同一 key 换到 params 与顶层同样被拒绝：两处各有独立的未知字段拦截。
    for input in [
        json!({
            "method": "load",
            "params": {"name": "example", "systemMcp": true, "config": {"command": "example-mcp"}}
        }),
        json!({
            "method": "load",
            "system_mcp": true,
            "params": {"name": "example", "config": {"command": "example-mcp"}}
        }),
    ] {
        assert!(
            tool.bind_invocation(input).is_err(),
            "System key 出现在 config 之外的位置时必须同样被拒绝"
        );
    }
    assert!(deployment.actions.lock().unwrap().is_empty());
}

struct RecordingRejectBroker {
    names: Mutex<Vec<String>>,
}

#[async_trait]
impl UserInteractionBroker for RecordingRejectBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        if let InteractionContext::Approval { items } = context {
            self.names
                .lock()
                .unwrap()
                .extend(items.iter().map(|item| item.tool_name.clone()));
            InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Reject {
                        reason: "rejected".to_string(),
                        source: None,
                    })
                    .collect(),
            )
        } else {
            InteractionResponse::Decisions(Vec::new())
        }
    }
}

#[tokio::test]
async fn permission_broker_observes_method_level_name_before_side_effects() {
    let deployment = Arc::new(FakeDeployment::default());
    let tool = DynamicMcpTool::new(
        "session-a",
        Arc::clone(&deployment) as Arc<dyn DynamicMcpDeploymentPort>,
    );
    let bound = tool.bind_invocation(load_input()).unwrap().unwrap();
    let broker = Arc::new(RecordingRejectBroker {
        names: Mutex::new(Vec::new()),
    });
    let permission = crate::permission::PermissionMiddleware::new(
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        crate::permission::default_requires_approval,
    );
    let call = ToolCall::new("call-approval", bound.policy_name, bound.policy_input);

    let result = permission
        .before_tool(&mut AgentState::new("/tmp"), &call)
        .await;

    assert!(result.is_err());
    assert_eq!(broker.names.lock().unwrap().as_slice(), ["DynamicMCP.load"]);
    assert!(deployment.actions.lock().unwrap().is_empty());
}

#[test]
fn status_is_read_only_policy_identity() {
    let deployment = Arc::new(FakeDeployment::default());
    let tool = DynamicMcpTool::new("session-a", deployment as Arc<dyn DynamicMcpDeploymentPort>);
    let bound = tool
        .bind_invocation(json!({"method": "status", "params": {}}))
        .unwrap()
        .unwrap();
    assert_eq!(bound.policy_name, "DynamicMCP.status");
    assert!(!crate::permission::default_requires_approval(
        &bound.policy_name
    ));
}
