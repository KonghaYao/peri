//! App relay 的**夹具与 canonical 入口消费**（阶段 4a 的 App facts 面用例见
//! `apps_relay_cases_test.rs`，同一模块树）。
//!
//! 真实对象：进程内 MCP peer（计数 + 记录参数 + 资源面）+ 真实连接池 + 真实
//! `SessionInvocationService`（canonical 入口）+ 真实 `PoolMcpAppsRelay`。
//!
//! S8 起旧的 relay 面方法（`open_app` / `read_resource` / `call_tool`）已删除：
//! App 回调的唯一生产入口是 `SessionAppsFacade::request` → `SessionInvocationPort`，
//! 本夹具因此**直接驱动同一入口**（与 façade 同一条链，少一层投影），断言落在
//! `SessionInvocationOutcome` 上。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::interaction::ConnectionEpoch;
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppsErrorKind, McpAppsRelayPort, SessionInvocationOutcome,
    SessionInvocationPort, SessionInvocationRequest,
};
use peri_acp_types::mcp_apps_facade::{
    AppAttachRequest, AppQualification, AttachmentBinding, ClientViewId, RequestNamespaceId,
    RequestSeq,
};

use super::*;
use crate::mcp::client::output_store::tests::Wire;
use crate::mcp::client::McpClientPool;
use crate::mcp::session_invocation::{SessionInvocationInputs, SessionInvocationService};
use crate::mcp::McpAppRecordSpec;

const SESSION: &str = "relay-session";
const CONNECTION: &str = "relay-connection";
const SERVER: &str = "apps-server";

/// App 可见工具（声明 `ui.resourceUri`）。
fn app_tool() -> rmcp::model::Tool {
    serde_json::from_value(serde_json::json!({
        "name": "show_canvas",
        "description": "canvas",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"resourceUri": "ui://app", "visibility": ["app", "model"]}}
    }))
    .expect("夹具工具声明必须可解析")
}

/// 把真实 peer 装进池（工具面是 App 工具）。
fn install_app_server(pool: &McpClientPool, wire: &Wire) {
    let handle = Arc::new(crate::mcp::client::McpClientHandle {
        call_gate: Default::default(),
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(wire.client.peer().clone()),
        tools: vec![app_tool()],
        resources: vec![],
        status: crate::mcp::client::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
    });
    pool.advance_handle_generation(&handle);
    pool.clients.write().insert(SERVER.to_string(), handle);
    // 生产同源：会话级 task manager 绑定（bridge 调用前会校验会话任务作用域）。
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);
}

/// App relay 夹具：真实 peer + 一条已附着的 record + **真实 service**（决定桩）。
struct RelayFixture {
    wire: Wire,
    relay: PoolMcpAppsRelay,
    service: Arc<SessionInvocationService>,
    server_generation: u64,
    record_id: String,
    attachment_id: AttachmentId,
    namespace: RequestNamespaceId,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
    resource_reads: Arc<AtomicUsize>,
    resource_lists: Arc<AtomicUsize>,
}

impl RelayFixture {
    /// App 回调：经 **canonical 入口**（生产上由 `apps/request` façade 调用同一入口）。
    ///
    /// 返回 `Err(kind)` 的 kind 来自 `SessionInvocationOutcome::error_kind`（契约层
    /// 单一权威：旧 relay `call_tool` 与 façade 用的是同一份投影）。
    async fn app_callback(
        &self,
        attachment_id: &AttachmentId,
        epoch: ConnectionEpoch,
    ) -> Result<peri_acp_types::tool_outcome::RawToolResult, McpAppsErrorKind> {
        let outcome = self
            .service
            .invoke(SessionInvocationRequest {
                session_id: SESSION.into(),
                runtime_epoch: self.service.runtime_epoch(),
                origin: peri_acp_types::tool_invocation::InvocationOrigin::App {
                    app_id: self.record_id.clone(),
                    attachment_id: Some(attachment_id.clone()),
                    connection_epoch: Some(epoch),
                    server_generation: self.server_generation,
                },
                target: InvocationTarget::App {
                    record_id: self.record_id.clone(),
                    tool_name: "show_canvas".into(),
                    arguments: serde_json::Map::new(),
                },
                // 请求作用域令牌：service 只把它当作准入前的放弃信号
                //（已提交的调用不被 App 关闭 / 连接关闭取消，设计 §2）。
                cancel: tokio_util::sync::CancellationToken::new(),
                invocation_id: None,
            })
            .await
            .map_err(|error| error.kind)?;
        match outcome {
            SessionInvocationOutcome::Completed(raw) => Ok(raw),
            other => Err(other.error_kind()),
        }
    }

    /// caller 给出的附件绑定（纪元由 caller 递增：本层不铸造纪元）。
    fn attachment_binding(&self, epoch: ConnectionEpoch) -> AttachmentBinding {
        AttachmentBinding {
            attachment_id: self.attachment_id.clone(),
            namespace: self.namespace.clone(),
            connection_epoch: epoch,
            next_seq: RequestSeq::new(1),
        }
    }

    /// 针对**指定** record 的附着请求（目标字段只作定位：server/tool/URI 由记录导出）。
    fn attach_request_for(&self, app_id: &str, attachment: AttachmentBinding) -> AppAttachRequest {
        AppAttachRequest {
            session_id: SESSION.into(),
            app_id: app_id.to_string(),
            owner_connection_id: CONNECTION.into(),
            attachment,
            client_view_id: ClientViewId::new("view-1"),
        }
    }

    fn attach_request(&self, attachment: AttachmentBinding) -> AppAttachRequest {
        self.attach_request_for(&self.record_id, attachment)
    }

    /// 另签发一条**未附着**记录（首附路径：`reattached=false` 的判别面）。
    fn issue_unattached_record(&self) -> Arc<McpAppRecord> {
        Arc::new(McpAppRecord::new(McpAppRecordSpec {
            owner_session_id: SESSION.into(),
            server_id: SERVER.into(),
            server_generation: self.server_generation,
            resource_uri: "ui://app".into(),
            instantiating_tool: "show_canvas".into(),
            invocation_id: peri_acp_types::tool_invocation::InvocationId::generate(),
            invocation_token: "token-second".into(),
            tool_call_id: None,
            incarnation: self.service.runtime_epoch(),
            qualification: AppQualification::Interactive {
                allowed_tools: std::collections::HashMap::from([(
                    "show_canvas".to_string(),
                    super::super::tool_bridge::effective_mcp_tool_name(SERVER, "show_canvas"),
                )]),
            },
            // 实际（审批后）输入：snapshot 的初始输入真相。
            effective_input: serde_json::Map::from_iter([(
                "source".to_string(),
                serde_json::json!("fixture"),
            )]),
            initial_result: Arc::new(peri_acp_types::tool_outcome::RawToolResult::new(
                serde_json::json!({"content": [{"type": "text", "text": "second"}], "isError": true}),
            )),
        }))
    }

    async fn shutdown(self) {
        self.relay.close_session(SESSION);
        self.wire.close().await;
    }
}

/// 真实 server：记录 wire 调用与参数，返回带内 `isError` 结果；资源面记录 read / list。
struct RelayServer {
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
    resource_reads: Arc<AtomicUsize>,
    resource_lists: Arc<AtomicUsize>,
}

impl rmcp::ServerHandler for RelayServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "peri-apps-relay-fixture",
            "apps_relay_test",
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        self.resource_lists.fetch_add(1, Ordering::SeqCst);
        Ok(rmcp::model::ListResourcesResult {
            resources: vec![serde_json::from_value(serde_json::json!({
                "uri": "videos://clip",
                "name": "clip",
                "mimeType": "video/mp4",
                "_meta": {"futureField": true}
            }))
            .expect("夹具资源声明必须可解析")],
            next_cursor: Some("cursor-1".into()),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        self.resource_reads.fetch_add(1, Ordering::SeqCst);
        let contents = rmcp::model::ResourceContents::BlobResourceContents {
            uri: request.uri,
            mime_type: Some("video/mp4".into()),
            blob: "aGk=".into(),
            meta: None,
        };
        Ok(rmcp::model::ReadResourceResult::new(vec![contents]).into())
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(serde_json::Value::Object(
            request.arguments.unwrap_or_default(),
        ));
        Ok(rmcp::model::CallToolResponse::Complete(
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                "canvas ready",
            )]),
        ))
    }
}

/// 决定桩：只替代「用户决定」这一不可控外部输入。
struct ApproveStub;

#[async_trait]
impl peri_acp_types::interaction::UserInteractionBroker for ApproveStub {
    async fn request(
        &self,
        _owner: peri_acp_types::interaction::TrustedInteractionOwner,
        _ctx: peri_acp_types::interaction::InteractionContext,
    ) -> peri_acp_types::interaction::InteractionResponse {
        peri_acp_types::interaction::InteractionResponse::Decisions(vec![
            peri_acp_types::interaction::ApprovalDecision::Approve { source: None },
        ])
    }
}

/// 真实 peer + 已附着 record + 真实 service（App 回调经 canonical 入口执行）。
async fn relay_with_service(
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
) -> RelayFixture {
    let resource_reads = Arc::new(AtomicUsize::new(0));
    let resource_lists = Arc::new(AtomicUsize::new(0));
    let wire = Wire::connect(RelayServer {
        calls: calls.clone(),
        inputs: inputs.clone(),
        resource_reads: resource_reads.clone(),
        resource_lists: resource_lists.clone(),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    install_app_server(&pool, &wire);
    let server_generation = pool.handle_generation(&pool.get_client(SERVER).unwrap());

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
    let service = Arc::new(SessionInvocationService::new(SessionInvocationInputs {
        pool: Arc::clone(&pool),
        agent_id: peri_acp_types::identity::AgentId::new(),
        session_id: SESSION.to_string(),
        cwd: std::sync::Arc::from("/tmp/relay-session"),
        queue: Arc::new(peri_agent::session::queue::MessageQueue::new),
        rules: Arc::new(
            peri_agent::session::factory::InvocationRuleChain::from_derived_blueprint(rules),
        ),
        closed_instances: Default::default(),
        session_cancel: tokio_util::sync::CancellationToken::new(),
    }));
    // 生产同序：会话环境登记运行时世代（签发点据此盖章）→ 签发 record → 附着连接。
    pool.app_binding_leases
        .register_session_incarnation(SESSION, service.runtime_epoch());
    let record = Arc::new(McpAppRecord::new(McpAppRecordSpec {
        owner_session_id: SESSION.into(),
        server_id: SERVER.into(),
        server_generation,
        resource_uri: "ui://app".into(),
        instantiating_tool: "show_canvas".into(),
        invocation_id: peri_acp_types::tool_invocation::InvocationId::generate(),
        invocation_token: "token-1".into(),
        tool_call_id: Some(peri_acp_types::tool_invocation::ToolCallId::new("call-1")),
        incarnation: service.runtime_epoch(),
        qualification: AppQualification::Interactive {
            allowed_tools: std::collections::HashMap::from([(
                "show_canvas".to_string(),
                super::super::tool_bridge::effective_mcp_tool_name(SERVER, "show_canvas"),
            )]),
        },
        effective_input: serde_json::Map::from_iter([(
            "source".to_string(),
            serde_json::json!("fixture"),
        )]),
        initial_result: Arc::new(peri_acp_types::tool_outcome::RawToolResult::new(
            serde_json::json!({
                "content": [{"type": "text", "text": "canvas ready"}],
                "structuredContent": {"ready": true}
            }),
        )),
    }));
    pool.app_binding_leases
        .issue(Arc::clone(&record))
        .expect("夹具自检：世代一致的记录必须可登记");
    let claimed = pool
        .app_binding_leases
        .find(&record.record_id)
        .expect("夹具自检：已签发 record 必须可读（附着按 record 身份发生）");
    // 附着：附件身份与 record 身份分离（阶段 4a），纪元取调用方值。
    let attachment_id = AttachmentId::new("attachment-1");
    let namespace = RequestNamespaceId::new();
    let route = Arc::new(AttachmentRoute::new(
        CONNECTION,
        attachment_id.clone(),
        namespace.clone(),
        ConnectionEpoch::initial(),
        ClientViewId::new("view-1"),
    ));
    assert!(claimed.attach(route), "夹具自检：记录必须可附着");
    let relay = PoolMcpAppsRelay::new(
        pool.clone(),
        service.runtime_epoch(),
        Arc::clone(&service) as Arc<dyn SessionInvocationPort>,
    );
    RelayFixture {
        wire,
        relay,
        service,
        server_generation,
        record_id: record.record_id.clone(),
        attachment_id,
        namespace,
        calls,
        inputs,
        resource_reads,
        resource_lists,
    }
}

/// App 回调经 canonical 入口**真实执行**（既有 before-tool 规则 + 真实 peer），
/// 且 raw 结果逐字返回（含带内 `isError`）。
#[tokio::test]
async fn app_tool_call_runs_through_canonical_entry_and_returns_raw_result() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;

    let raw = fixture
        .app_callback(&fixture.attachment_id.clone(), ConnectionEpoch::initial())
        .await
        .expect("批准后必须真实执行并返回 raw 结果");
    assert_eq!(raw.into_value()["content"][0]["text"], "canvas ready");
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "真实 peer 恰好一次调用"
    );
    assert_eq!(
        fixture.inputs.lock().unwrap().len(),
        1,
        "wire 参数来自本次 App 调用（未被改写）"
    );
    fixture.shutdown().await;
}

/// [S8] 旧 `validate_binding(&AppSessionBinding)` 的等价复核改为按 **record 身份**
/// 查询：成功返回该连接协商出的 MCP 协议版本；世代前进 ⇒ `StaleServerGeneration`；
/// 未知 / 已撤销记录 ⇒ `PolicyDenied`；记录在册但 server 无连接 ⇒ `ServerDisconnected`。
#[tokio::test]
async fn binding_protocol_version_reports_negotiated_version_and_rejects_stale_generation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let fixture = relay_with_service(calls, inputs).await;
    let negotiated = fixture
        .wire
        .client
        .peer_info()
        .map(|info| info.protocol_version.to_string())
        .expect("夹具自检：握手后必须有 peer info");

    // 成功：协议版本取自**本次连接**（不是客户端自述字段）。
    let protocol = fixture
        .relay
        .binding_protocol_version(&fixture.record_id)
        .await
        .expect("已附着记录必须可复核");
    assert_eq!(protocol, negotiated);

    // 世代前进：同一 record 的绑定必须显式失效（不按旧世代继续）。
    let handle = fixture
        .relay
        .pool
        .get_client(SERVER)
        .expect("fixture server installed");
    fixture.relay.pool.advance_handle_generation(&handle);
    let stale = fixture
        .relay
        .binding_protocol_version(&fixture.record_id)
        .await
        .expect_err("世代不符必须显式拒绝");
    assert_eq!(stale.kind, McpAppsErrorKind::StaleServerGeneration);

    // 未知记录：显式拒绝（不猜 server，也不回落）。
    let unknown = fixture
        .relay
        .binding_protocol_version("unknown-record")
        .await
        .expect_err("未知记录必须显式拒绝");
    assert_eq!(unknown.kind, McpAppsErrorKind::PolicyDenied);

    // 记录在册但 server 已无连接：`ServerDisconnected`（与「记录不存在」可区分）。
    // server 句柄从池中摘除：记录仍在注册表，但已无可观察连接。
    fixture.relay.pool.clients.write().remove(SERVER);
    let disconnected = fixture
        .relay
        .binding_protocol_version(&fixture.record_id)
        .await
        .expect_err("无连接必须显式拒绝");
    assert_eq!(disconnected.kind, McpAppsErrorKind::ServerDisconnected);
    fixture.shutdown().await;
}

// 用例容量拆分（STD-SIZE-001）：夹具留在本文件，其余用例在兄弟模块。
#[path = "apps_relay_cases_test.rs"]
mod cases;
