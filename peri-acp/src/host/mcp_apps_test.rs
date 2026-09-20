use super::*;
use peri_acp_types::mcp_apps::{
    AppSessionBinding, JsonRpcRequest, JsonRpcResponse, McpAppInvokeOutcome, McpAppOpenRequest,
    McpAppsRelayError, McpAppsRelayPort, RawResource,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct StubRelay {
    invoke: std::sync::Mutex<Option<Result<McpAppInvokeOutcome, McpAppsErrorKind>>>,
}

impl StubRelay {
    fn invoke_ok() -> Self {
        Self {
            invoke: std::sync::Mutex::new(Some(Ok(McpAppInvokeOutcome {
                mcp_protocol_version: "2025-03-26".into(),
                tool_call_id: "tool-new-1".into(),
                effective_tool_name: "mcp__cursor-canvas__show_canvas".into(),
                arguments: serde_json::Map::from_iter([(
                    "source".into(),
                    json!("export default function App(){return null}"),
                )]),
                output: "ok".into(),
            }))),
        }
    }
}

#[async_trait::async_trait]
impl McpAppsRelayPort for StubRelay {
    fn close_connection(&self, _: &str) {}
    fn close_session(&self, _: &str) {}
    fn begin_session_turn(&self, _: &str) {}

    async fn open_app(
        &self,
        _: &str,
        _: &McpAppOpenRequest,
    ) -> Result<(String, AppSessionBinding), McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn validate_binding(&self, _: &AppSessionBinding) -> Result<String, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn read_resource(
        &self,
        _: &AppSessionBinding,
    ) -> Result<(String, Vec<RawResource>), McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn call_tool(
        &self,
        _: &AppSessionBinding,
        _: JsonRpcRequest,
    ) -> Result<(String, JsonRpcResponse), McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn invoke_app(
        &self,
        _: &McpAppInvokeRequest,
        _: CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        match self.invoke.lock().unwrap().take() {
            Some(Ok(outcome)) => Ok(outcome),
            Some(Err(kind)) => Err(McpAppsRelayError { kind }),
            None => Err(McpAppsRelayError {
                kind: McpAppsErrorKind::UpstreamProtocolError,
            }),
        }
    }
}

fn enabled_connection() -> ConnectionContext {
    let mut connection = ConnectionContext::new(true);
    connection.commit_initialize();
    connection
}

fn invoke_params() -> Value {
    json!({
        "envelopeVersion": MCP_APPS_ENVELOPE_VERSION,
        "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
        "serverId": "cursor-canvas",
        "toolName": "show_canvas",
        "ownerSessionId": "acp-session-1",
        "arguments": {"source": "export default function App(){return null}"}
    })
}

#[test]
fn version_validation_is_fail_closed() {
    assert!(validate_versions(MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION).is_ok());
    let error = validate_versions("2", MCP_APPS_PROTOCOL_VERSION).unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "unsupported_envelope_version");
}

#[test]
fn initial_binding_uses_stable_apps_version() {
    let binding = initial_binding(
        "connection".into(),
        "session".into(),
        "server".into(),
        7,
        "ui://app".into(),
        "open".into(),
    );
    assert_eq!(binding.server_generation, 7);
    assert_eq!(binding.apps_protocol_version, MCP_APPS_PROTOCOL_VERSION);
}

#[tokio::test]
async fn invoke_disabled_capability_fails_closed() {
    let mut connection = ConnectionContext::new(false);
    connection.commit_initialize();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        InvokeSessionGate {
            known: true,
            owned: true,
            prompt_in_flight: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "capability_disabled");
}

#[tokio::test]
async fn invoke_unknown_session_fails_closed() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        InvokeSessionGate {
            known: false,
            owned: false,
            prompt_in_flight: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "invalid_session");
}

#[tokio::test]
async fn invoke_during_prompt_is_policy_denied() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        InvokeSessionGate {
            known: true,
            owned: true,
            prompt_in_flight: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "policy_denied");
}

/// 只读准入的会话没有执行所有权，不是可执行对象：invoke 与 prompt 同属执行面，闸门必须
/// 在没有 owner 时拒绝，而不是因为 `known` 为真就执行工具并向该会话下发 session/update。
#[tokio::test]
async fn invoke_without_execution_ownership_is_policy_denied() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        InvokeSessionGate {
            known: true,
            owned: false,
            prompt_in_flight: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "policy_denied");
}

#[tokio::test]
async fn invoke_success_projects_completed_tool_call_and_returns_tool_call_id() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let result = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        InvokeSessionGate {
            known: true,
            owned: true,
            prompt_in_flight: false,
        },
    )
    .await
    .expect("invoke");
    assert_eq!(result.session_id, "acp-session-1");
    assert_eq!(result.value["toolCallId"], "tool-new-1");
    assert_eq!(result.value["serverId"], "cursor-canvas");
    assert_eq!(result.updates.len(), 2);
    match &result.updates[0] {
        SessionUpdate::ToolCall(call) => {
            assert_eq!(call.tool_call_id.0.as_ref(), "tool-new-1");
            assert_eq!(call.title, "mcp__cursor-canvas__show_canvas");
            assert_eq!(
                call.status,
                agent_client_protocol::schema::v1::ToolCallStatus::InProgress
            );
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
    match &result.updates[1] {
        SessionUpdate::ToolCallUpdate(update) => {
            assert_eq!(update.tool_call_id.0.as_ref(), "tool-new-1");
            assert_eq!(
                update.fields.status,
                Some(agent_client_protocol::schema::v1::ToolCallStatus::Completed)
            );
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[tokio::test]
async fn invoke_unknown_method_through_request_router_is_unsupported() {
    let mut connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_request(
        "peri/mcp/invoke",
        &invoke_params(),
        &mut connection,
        Some(&relay),
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "unsupported_method");
}
