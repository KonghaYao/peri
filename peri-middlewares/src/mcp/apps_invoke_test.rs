use super::*;
use crate::mcp::client::{ClientStatus, McpClientHandle, McpClientPool};
/// Host 归属值：生产上由 `SessionAppsFacade` 的 Host 操作域 + 请求账本给出
/// （序号权威在调用方）；本文件只驱动 relay 面，故显式给出一对固定值。
fn host_attribution() -> peri_acp_types::mcp_apps::HostInvocationRef {
    peri_acp_types::mcp_apps::HostInvocationRef {
        namespace: "peri/mcp/invoke".to_string(),
        seq: 1,
    }
}

use peri_acp_types::mcp_apps::{
    McpAppInvokeRequest, McpAppsRelayPort, MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use rmcp::model::Tool;
use serde_json::json;
use std::sync::Arc;

fn invoke_request(server_id: &str, tool_name: &str, session: &str) -> McpAppInvokeRequest {
    McpAppInvokeRequest {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        server_id: server_id.into(),
        tool_name: tool_name.into(),
        owner_session_id: session.into(),
        arguments: serde_json::Map::from_iter([(
            "source".into(),
            json!("export default function App(){return null}"),
        )]),
    }
}

fn ui_tool(name: &str, resource: Option<&str>, visibility: &[&str]) -> Tool {
    let mut ui = serde_json::Map::new();
    ui.insert("visibility".into(), json!(visibility));
    if let Some(resource) = resource {
        ui.insert("resourceUri".into(), json!(resource));
    }
    serde_json::from_value(json!({
        "name": name,
        "description": name,
        "inputSchema": {"type": "object"},
        "_meta": {"ui": ui}
    }))
    .unwrap()
}

fn insert_server(pool: &McpClientPool, name: &str, tools: Vec<Tool>) {
    pool.clients.write().insert(
        name.to_string(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: name.to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
}

/// 预准入测试专用 relay：canonical 入口**不可达**（调用即 panic）。
///
/// 存在的意义是证据：请求级校验（取消 / 空身份 / 未知 server / 工具不可见 /
/// 断连）必须在触碰 canonical 入口**之前**完成——若实现把校验挪到入口之后，
/// 这些用例会立刻 panic。
fn pre_admission_relay(pool: Arc<McpClientPool>) -> PoolMcpAppsRelay {
    struct UnreachableInvocationPort;

    #[::async_trait::async_trait]
    impl peri_acp_types::mcp_apps::SessionInvocationPort for UnreachableInvocationPort {
        async fn invoke(
            &self,
            _request: peri_acp_types::mcp_apps::SessionInvocationRequest,
        ) -> Result<
            peri_acp_types::mcp_apps::SessionInvocationOutcome,
            peri_acp_types::mcp_apps::McpAppsRelayError,
        > {
            unreachable!("预准入校验必须先于 canonical 入口完成")
        }

        fn session_closing(&self) {}

        fn revoke_app(&self, _app_id: &str) {}

        fn revoke_attachment(&self, _attachment_id: &peri_acp_types::interaction::AttachmentId) {}

        fn source_target(
            &self,
            _session_id: &str,
            _source: &peri_acp_types::tool_invocation::InvocationId,
        ) -> Option<peri_acp_types::mcp_apps_facade::TargetIdentity> {
            unreachable!("预准入校验必须先于 canonical 入口完成")
        }
    }

    PoolMcpAppsRelay::new(pool, 1, Arc::new(UnreachableInvocationPort))
}

async fn invoke_kind(pool: McpClientPool, request: McpAppInvokeRequest) -> McpAppsErrorKind {
    let relay = pre_admission_relay(Arc::new(pool));
    relay
        .invoke_app_inner(&request, host_attribution(), CancellationToken::new())
        .await
        .unwrap_err()
        .kind
}

#[tokio::test]
async fn invoke_unknown_server_fails_closed() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        invoke_kind(pool, invoke_request("missing", "show_canvas", "session")).await,
        McpAppsErrorKind::UnknownServer
    );
}

#[tokio::test]
async fn invoke_empty_session_fails_closed() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        invoke_kind(pool, invoke_request("cursor-canvas", "show_canvas", "  ")).await,
        McpAppsErrorKind::InvalidSession
    );
}

#[tokio::test]
async fn invoke_missing_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool("other", Some("ui://app"), &["app"])],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ToolNotFound
    );
}

#[tokio::test]
async fn invoke_model_only_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool(
            "show_canvas",
            Some("ui://cursor-canvas/mcp-app.html"),
            &["model"],
        )],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ToolNotAppVisible
    );
}

#[tokio::test]
async fn invoke_tool_without_ui_resource_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool("show_canvas", None, &["app"])],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::InvalidResource
    );
}

#[tokio::test]
async fn invoke_disconnected_app_tool_fails_closed() {
    let pool = McpClientPool::new_empty();
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool(
            "show_canvas",
            Some("ui://cursor-canvas/mcp-app.html"),
            &["app"],
        )],
    );
    assert_eq!(
        invoke_kind(
            pool,
            invoke_request("cursor-canvas", "show_canvas", "session")
        )
        .await,
        McpAppsErrorKind::ServerDisconnected
    );
}

/// [审查回归] Host invoke 的准入视图必须携带**会话真实关闭集**：被 MetaHarness
/// 关闭的 builtin 实例工具不可解析（`TargetUnbound` ⇒ 显式拒绝，零 wire 调用）。
#[tokio::test]
async fn host_invoke_cannot_resolve_a_closed_builtin_instance() {
    use async_trait::async_trait;
    use peri_acp_types::interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
        UserInteractionBroker,
    };

    struct ApproveStub;

    #[async_trait]
    impl UserInteractionBroker for ApproveStub {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            _ctx: InteractionContext,
        ) -> InteractionResponse {
            InteractionResponse::Decisions(vec![ApprovalDecision::Approve { source: None }])
        }
    }

    struct CountingServer {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl rmcp::ServerHandler for CountingServer {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            rmcp::model::ServerConfig::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
            .with_server_info(rmcp::model::Implementation::new(
                "peri-closed-instance-fixture",
                "apps_invoke_test",
            ))
        }

        async fn call_tool(
            &self,
            _: rmcp::model::CallToolRequestParams,
            _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(rmcp::model::CallToolResponse::Complete(
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                    "should not be reached",
                )]),
            ))
        }
    }

    const SESSION: &str = "closed-instance-session";
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = crate::mcp::client::output_store::tests::Wire::connect(CountingServer {
        calls: Arc::clone(&calls),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    insert_server(
        &pool,
        "workspace",
        vec![ui_tool(
            "folder_operations",
            Some("ui://app"),
            &["app", "model"],
        )],
    );
    {
        let mut clients = pool.clients.write();
        let handle = Arc::get_mut(clients.get_mut("workspace").unwrap()).unwrap();
        handle.peer = Some(wire.client.peer().clone());
        handle.source = Some(crate::mcp::config::ConfigSource::Builtin {
            instance: "workspace".to_string(),
        });
    }
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);

    let mut rules = peri_agent::middleware::chain::MiddlewareChain::new();
    rules.add(Box::new(
        crate::permission::PermissionMiddleware::with_shared_mode(
            Arc::new(ApproveStub),
            crate::permission::default_requires_approval,
            peri_acp_types::permission::SharedPermissionMode::new(
                peri_acp_types::permission::PermissionMode::Default,
            ),
            None,
        ),
    ));
    let service = Arc::new(
        crate::mcp::session_invocation::SessionInvocationService::new(
            crate::mcp::session_invocation::SessionInvocationInputs {
                pool: Arc::clone(&pool),
                agent_id: peri_acp_types::identity::AgentId::new(),
                session_id: SESSION.to_string(),
                cwd: std::sync::Arc::from("/tmp/closed-instance"),
                queue: Arc::new(peri_agent::session::queue::MessageQueue::new),
                rules: Arc::new(
                    peri_agent::session::factory::InvocationRuleChain::from_derived_blueprint(
                        rules,
                    ),
                ),
                // 会话关闭集：workspace 实例被 MetaHarness 关闭。
                closed_instances: std::iter::once("workspace".to_string()).collect(),
                session_cancel: CancellationToken::new(),
            },
        ),
    );
    let relay = PoolMcpAppsRelay::new(
        Arc::clone(&pool),
        service.runtime_epoch(),
        Arc::clone(&service) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
    );

    let error = relay
        .invoke_app(
            &invoke_request("workspace", "folder_operations", SESSION),
            host_attribution(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        McpAppsErrorKind::PolicyDenied,
        "关闭的实例工具必须在准入视图之外"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "不可解析的目标不得发出 wire 调用"
    );
    assert_eq!(
        service.outcomes().latest().map(|record| record.outcome),
        Some(
            peri_acp_types::mcp_apps::SessionInvocationOutcome::NotExecuted {
                cause: peri_acp_types::mcp_apps::NotExecutedCause::TargetUnbound,
            }
        )
    );
    pool.clients.write().clear();
    wire.close().await;
}

#[tokio::test]
async fn cancelled_host_invoke_fails_before_admission() {
    let relay = pre_admission_relay(Arc::new(McpClientPool::new_empty()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = relay
        .invoke_app(
            &invoke_request("app-owner", "large", "child"),
            host_attribution(),
            cancellation,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, McpAppsErrorKind::Cancelled);
}

/// Host invoke 经共同入口**真实执行**（设计 §4.1 与裁决「终点状态必须功能可用」：
/// 不再恒 `PolicyDenied`），回执返回本次执行身份作为 `toolCallId`。
#[tokio::test]
async fn host_invoke_runs_through_canonical_entry() {
    use async_trait::async_trait;
    use peri_acp_types::interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
        UserInteractionBroker,
    };

    /// 决定桩：只替代「用户决定」这一不可控外部输入。
    struct ApproveStub;

    #[async_trait]
    impl UserInteractionBroker for ApproveStub {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            _ctx: InteractionContext,
        ) -> InteractionResponse {
            InteractionResponse::Decisions(vec![ApprovalDecision::Approve { source: None }])
        }
    }

    struct CountingServer {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl rmcp::ServerHandler for CountingServer {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            rmcp::model::ServerConfig::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
            .with_server_info(rmcp::model::Implementation::new(
                "peri-host-invoke-fixture",
                "apps_invoke_test",
            ))
        }

        async fn call_tool(
            &self,
            _: rmcp::model::CallToolRequestParams,
            _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(rmcp::model::CallToolResponse::Complete(
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                    "host invoke ok",
                )]),
            ))
        }
    }

    const SESSION: &str = "host-invoke-session";
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = crate::mcp::client::output_store::tests::Wire::connect(CountingServer {
        calls: Arc::clone(&calls),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    insert_server(
        &pool,
        "cursor-canvas",
        vec![ui_tool("show_canvas", Some("ui://app"), &["app", "model"])],
    );
    {
        let mut clients = pool.clients.write();
        let handle = Arc::get_mut(clients.get_mut("cursor-canvas").unwrap()).unwrap();
        handle.peer = Some(wire.client.peer().clone());
        handle.tools = vec![ui_tool("show_canvas", Some("ui://app"), &["app", "model"])];
    }
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);

    let mut rules = peri_agent::middleware::chain::MiddlewareChain::new();
    rules.add(Box::new(
        crate::permission::PermissionMiddleware::with_shared_mode(
            Arc::new(ApproveStub),
            crate::permission::default_requires_approval,
            peri_acp_types::permission::SharedPermissionMode::new(
                peri_acp_types::permission::PermissionMode::Default,
            ),
            None,
        ),
    ));
    let service = Arc::new(
        crate::mcp::session_invocation::SessionInvocationService::new(
            crate::mcp::session_invocation::SessionInvocationInputs {
                pool: Arc::clone(&pool),
                agent_id: peri_acp_types::identity::AgentId::new(),
                session_id: SESSION.to_string(),
                cwd: std::sync::Arc::from("/tmp/host-invoke"),
                queue: Arc::new(peri_agent::session::queue::MessageQueue::new),
                rules: Arc::new(
                    peri_agent::session::factory::InvocationRuleChain::from_derived_blueprint(
                        rules,
                    ),
                ),
                closed_instances: Default::default(),
                session_cancel: CancellationToken::new(),
            },
        ),
    );
    let relay = PoolMcpAppsRelay::new(
        Arc::clone(&pool),
        service.runtime_epoch(),
        Arc::clone(&service) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
    );

    let mut request = invoke_request("cursor-canvas", "show_canvas", SESSION);
    request.arguments.clear();
    let outcome = relay
        .invoke_app(&request, host_attribution(), CancellationToken::new())
        .await
        .expect("Host invoke 必须经 canonical 入口真实执行");
    assert_eq!(
        outcome.effective_tool_name,
        "mcp__cursor-canvas__show_canvas"
    );
    assert!(
        !outcome.tool_call_id.is_empty(),
        "回执必须带稳定的执行身份（toolCallId）"
    );
    assert!(
        outcome.output.contains("host invoke ok"),
        "回执必须携带真实结果载荷，实际: {}",
        outcome.output
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    pool.clients.write().clear();
    wire.close().await;
}
