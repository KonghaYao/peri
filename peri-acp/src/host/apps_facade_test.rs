//! [设计 §6 表 A2–A13 + §8.1 低 5 的 ACP 层] `peri/mcp/apps/*` 七个方法的真实路径回归。
//!
//! 真实对象：进程内 MCP peer（记录每次 `tools/call` 的工具名与参数、资源 read /
//! list）、真实连接池、真实 bridge/canonical 管线、真实 `SessionInvocationService`、
//! 真实 `SessionInteractionRouter`、真实 `PoolMcpAppsRelay`、真实 `RequestLedger`、
//! 真实 `SessionAppsFacade`，以及真实 ACP 分发（`host::mcp_apps`）。
//!
//! 被替代的只有两类不可控外部输入：
//! 1. ACP 客户端帧（stub transport 记录 `session/request_permission` 并按预设应答）；
//! 2. 模型侧派发器（只为签发 record 的实例化调用提供白名单解析）。
//!
//! 断言落在「wire 调用次数 + 工具名 + 判决/outcome 形状 + 审批帧数」上：判决必须
//! 按键可区分，未执行不得伪装成用户拒绝，同 key 不得二次执行。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex as StdMutex,
};

use async_trait::async_trait;
use peri_acp_types::mcp_apps::McpAppsRelayPort;
use peri_acp_types::mcp_apps_facade::{
    AppQualification, AppsAttachResponse, AppsRequestResponse, CancelVerdict, CloseVerdict,
    DetachVerdict, McpAppsFeatures, McpAppsLimits, RequestVerdict,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::tool_invocation::{InvocationId, InvocationOrigin};
use peri_acp_types::tools::{EffectiveToolDefinition, EffectiveToolDispatcher, ToolContext};
use peri_agent::session::factory::InvocationRuleChain;
use peri_middlewares::mcp::apps_relay::PoolMcpAppsRelay;
use peri_middlewares::mcp::client::{ClientStatus, McpClientHandle, McpClientPool, OAuthStatus};
use peri_middlewares::mcp::session_invocation::{
    SessionInvocationInputs, SessionInvocationService,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ServerCapabilities, ServerConfig,
};
use serde_json::{json, Value};

use super::*;
use crate::host::connection::ConnectionContext;
use crate::transport::types::{IncomingMessage, RequestId};
use crate::transport::AcpTransport;

const SESSION: &str = "facade-session";
const SERVER: &str = "cursor-canvas";
const APP_TOOL: &str = "show_canvas";
const APP_HELPER: &str = "app_only_helper";
const MODEL_TOOL: &str = "model_only_card";
const BROKEN_TOOL: &str = "broken_canvas";
const RESOURCE_URI: &str = "ui://app";
const MODEL_URI: &str = "ui://model";
const VIDEO_URI: &str = "videos://app/clip";
const LOCAL_URI: &str = "file:///etc/passwd";

// ─── ACP 客户端帧 stub（唯一替代的传输面） ──────────────────────────────────

#[derive(Default)]
struct FrameTransport {
    frames: StdMutex<Vec<(String, Value)>>,
    choice: StdMutex<Option<String>>,
}

impl FrameTransport {
    fn with_choice(choice: &str) -> Arc<Self> {
        let transport = Arc::new(Self::default());
        *transport.choice.lock().unwrap() = Some(choice.to_string());
        transport
    }

    fn frames(&self, method: &str) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    fn permission_frames(&self) -> Vec<Value> {
        self.frames("session/request_permission")
    }
}

#[async_trait]
impl AcpTransport for FrameTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.frames
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        match method {
            "session/request_permission" => {
                let choice = self
                    .choice
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "reject_once".to_string());
                Ok(json!({"outcome": {"outcome": "selected", "optionId": choice}}))
            }
            _ => Ok(Value::Null),
        }
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

// ─── 进程内 MCP peer ────────────────────────────────────────────────────────

struct CanvasServer {
    calls: Arc<AtomicUsize>,
    tool_calls: Arc<StdMutex<Vec<String>>>,
    resource_reads: Arc<AtomicUsize>,
    resource_lists: Arc<AtomicUsize>,
    /// `Some`：第 `gate_on_call` 次调用阻塞到 gate 释放（观察 in-flight 窗口）。
    gate: Option<Arc<tokio::sync::Notify>>,
    gate_on_call: usize,
}

impl rmcp::ServerHandler for CanvasServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "peri-canvas",
            "facade_test",
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.tool_calls
            .lock()
            .unwrap()
            .push(request.name.to_string());
        if call == self.gate_on_call {
            if let Some(gate) = self.gate.as_ref() {
                gate.notified().await;
            }
        }
        let result: CallToolResult = match request.name.as_ref() {
            BROKEN_TOOL => serde_json::from_value(json!({
                "content": [{"type": "text", "text": "canvas exploded"}],
                "isError": true
            })),
            APP_HELPER => serde_json::from_value(json!({
                "content": [{"type": "text", "text": "helper ran"}]
            })),
            _ => serde_json::from_value(json!({
                // 多模态 + structuredContent + `_meta`：snapshot 必须逐字保真。
                "content": [
                    {"type": "text", "text": "canvas ready"},
                    {"type": "image", "data": "aGk=", "mimeType": "image/png"}
                ],
                "structuredContent": {"canvas": {"width": 640}},
                "_meta": {"trace": "fixture-trace"}
            })),
        }
        .expect("夹具工具结果必须可解析");
        Ok(CallToolResponse::Complete(result))
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        self.resource_lists.fetch_add(1, Ordering::SeqCst);
        Ok(rmcp::model::ListResourcesResult {
            resources: vec![serde_json::from_value(json!({
                "uri": VIDEO_URI,
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
        // 读取只发给绑定 server：即使 URI 伪装成本机文件，也只有 server 的字节回来。
        let contents = rmcp::model::ResourceContents::BlobResourceContents {
            uri: request.uri,
            mime_type: Some("video/mp4".into()),
            blob: "aGk=".into(),
            meta: None,
        };
        Ok(rmcp::model::ReadResourceResult::new(vec![contents]).into())
    }
}

struct Peer {
    client: rmcp::service::RunningService<
        rmcp::service::RoleClient,
        rmcp::model::InitializeRequestParams,
    >,
    server: tokio::task::JoinHandle<()>,
}

impl Peer {
    async fn connect(handler: CanvasServer) -> Self {
        use rmcp::ServiceExt;
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handler
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client_info = rmcp::model::InitializeRequestParams::new(
            rmcp::model::ClientCapabilities::default(),
            rmcp::model::Implementation::new("peri-acp-facade-test", "0.1"),
        );
        let client = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client_info.serve(client_io),
        )
        .await
        .expect("client handshake timed out")
        .expect("client handshake failed");
        Self { client, server }
    }

    async fn close(mut self) {
        let _ = self
            .client
            .close_with_timeout(std::time::Duration::from_secs(1))
            .await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), self.server).await;
    }
}

/// UI 声明：`visibility` / `resourceUri` 决定签发资格与白名单（与生产同一解析）。
fn ui_tool(name: &str, visibility: &[&str], resource_uri: Option<&str>) -> rmcp::model::Tool {
    let mut meta = json!({});
    if let Some(resource_uri) = resource_uri {
        meta = json!({"ui": {"resourceUri": resource_uri, "visibility": visibility}});
    }
    serde_json::from_value(json!({
        "name": name,
        "description": name,
        "inputSchema": {"type": "object"},
        "_meta": meta,
    }))
    .expect("工具声明必须可解析")
}

fn server_tools() -> Vec<rmcp::model::Tool> {
    vec![
        ui_tool(APP_TOOL, &["app", "model"], Some(RESOURCE_URI)),
        ui_tool(APP_HELPER, &["app"], Some(RESOURCE_URI)),
        ui_tool(BROKEN_TOOL, &["app"], Some(RESOURCE_URI)),
        ui_tool(MODEL_TOOL, &["model"], Some(MODEL_URI)),
    ]
}

fn server_handle(peer: &Peer) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(peer.client.peer().clone()),
        tools: server_tools(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::None,
        source: None,
        url: None,
        skills_capable: false,
    })
}

/// 模型侧派发器桩：只回答「该 server 的 wire 名对应的 effective 名」。
struct AdmissionStub;

#[async_trait]
impl EffectiveToolDispatcher for AdmissionStub {
    async fn dispatch(
        &self,
        _call: peri_acp_types::tool_invocation::EffectiveToolCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<peri_acp_types::tools::ToolOutput, peri_acp_types::tools::EffectiveToolError> {
        unreachable!("测试不执行模型侧调用")
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        Vec::new()
    }

    fn admitted_mcp_tool_name(&self, server: &str, wire_name: &str) -> Option<String> {
        (server == SERVER).then(|| format!("mcp__{server}__{wire_name}"))
    }
}

// ─── 夹具 ──────────────────────────────────────────────────────────────────

struct Fixture {
    session: String,
    pool: Arc<McpClientPool>,
    service: Arc<SessionInvocationService>,
    relay: Arc<dyn McpAppsRelayPort>,
    router: Arc<SessionInteractionRouter>,
    facade: Arc<SessionAppsFacade>,
    peer: Peer,
    calls: Arc<AtomicUsize>,
    tool_calls: Arc<StdMutex<Vec<String>>>,
    resource_reads: Arc<AtomicUsize>,
    resource_lists: Arc<AtomicUsize>,
    transport: Arc<FrameTransport>,
    /// 已登记为在线 endpoint 的连接上下文（label → 上下文；label 只用于测试可读）。
    connections: Vec<(String, ConnectionContext)>,
    /// 任务作用域 owner：facade 的 spawner 持有它的 `Weak`，drop 后新任务被拒。
    _task_owner: crate::host::task_scope::HostTaskOwner,
}

impl Fixture {
    async fn new(transport: Arc<FrameTransport>) -> Self {
        Self::with_gate(transport, None).await
    }

    async fn with_gate(
        transport: Arc<FrameTransport>,
        gate: Option<Arc<tokio::sync::Notify>>,
    ) -> Self {
        Self::for_session(SESSION, transport, gate, 1).await
    }

    /// 两条在线连接（重挂接 / 连接替换场景）。
    async fn with_connections(transport: Arc<FrameTransport>, connections: usize) -> Self {
        Self::for_session(SESSION, transport, None, connections).await
    }

    /// `connections`：注册到 router 的在线连接数（`conn-1`、`conn-2`、…）。
    async fn for_session(
        session: &str,
        transport: Arc<FrameTransport>,
        gate: Option<Arc<tokio::sync::Notify>>,
        connections: usize,
    ) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(StdMutex::new(Vec::new()));
        let resource_reads = Arc::new(AtomicUsize::new(0));
        let resource_lists = Arc::new(AtomicUsize::new(0));
        let peer = Peer::connect(CanvasServer {
            calls: Arc::clone(&calls),
            tool_calls: Arc::clone(&tool_calls),
            resource_reads: Arc::clone(&resource_reads),
            resource_lists: Arc::clone(&resource_lists),
            gate,
            gate_on_call: 2,
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty_for_tests());
        pool.install_client_for_tests(server_handle(&peer));
        let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
        pool.bind_session_task_manager(session, &manager);

        let router = Arc::new(SessionInteractionRouter::new(
            session,
            Arc::new(tokio::sync::Mutex::new(())),
        ));
        // 连接上下文（服务端身份 = `ConnectionContext::id`）；label 只是测试可读名。
        let mut connection_contexts: Vec<(String, ConnectionContext)> = Vec::new();
        for index in 1..=connections {
            let mut context = ConnectionContext::new(true);
            context.commit_initialize();
            let erased: Arc<dyn AcpTransport> = Arc::clone(&transport) as Arc<dyn AcpTransport>;
            router
                .register_connection(context.id(), erased)
                .expect("测试夹具连接必须可注册");
            connection_contexts.push((format!("conn-{index}"), context));
        }

        // 规则链：与生产同一构造实现（Permission 槽位），broker = 真实 router。
        let mut rules = peri_agent::middleware::chain::MiddlewareChain::new();
        rules.add(Box::new(
            peri_middlewares::permission::PermissionMiddleware::with_shared_mode(
                Arc::clone(&router) as Arc<dyn peri_acp_types::interaction::UserInteractionBroker>,
                peri_middlewares::permission::default_requires_approval,
                SharedPermissionMode::new(PermissionMode::Default),
                None,
            ),
        ));
        let session_cancel = tokio_util::sync::CancellationToken::new();
        let service = Arc::new(SessionInvocationService::new(SessionInvocationInputs {
            pool: Arc::clone(&pool),
            agent_id: peri_acp_types::identity::AgentId::new(),
            session_id: session.to_string(),
            cwd: Arc::from("/tmp/facade-acp"),
            queue: Arc::new(peri_acp_types::session::MessageQueue::new),
            rules: Arc::new(InvocationRuleChain::from_derived_blueprint(rules)),
            closed_instances: Default::default(),
            session_cancel,
        }));
        let apps_relay = PoolMcpAppsRelay::new(
            Arc::clone(&pool),
            service.runtime_epoch(),
            Arc::clone(&service) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
        );
        apps_relay.register_session_incarnation(session);
        let relay: Arc<dyn McpAppsRelayPort> = Arc::new(apps_relay);
        let (task_owner, spawner) = crate::host::task_scope::HostTaskOwner::new();
        let facade = Arc::new(SessionAppsFacade::new(
            session,
            service.runtime_epoch(),
            Arc::clone(&router),
            Arc::clone(&relay),
            Arc::clone(&service) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
            spawner,
        ));
        Self {
            session: session.to_string(),
            pool,
            service,
            relay,
            router,
            facade,
            peer,
            calls,
            tool_calls,
            resource_reads,
            resource_lists,
            transport,
            connections: connection_contexts,
            _task_owner: task_owner,
        }
    }

    /// 测试 label（`conn-1`）对应的**服务端连接身份**。
    fn connection_id(&self, label: &str) -> String {
        self.connections
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, context)| context.id().to_string())
            .unwrap_or_else(|| panic!("未知测试连接: {label}"))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_tool_call(&self) -> Option<String> {
        self.tool_calls.lock().unwrap().last().cloned()
    }

    /// 经**真实 ACP 分发**调用一个 `peri/mcp/apps/*` / 旧别名方法（默认 conn-1）。
    async fn apps(&mut self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.apps_from("conn-1", method, params).await
    }

    /// 同前，但由指定连接发起（服务端连接身份取自真实的 `ConnectionContext`）。
    async fn apps_from(
        &mut self,
        label: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, AcpError> {
        let facade = Arc::clone(&self.facade);
        let index = self
            .connections
            .iter()
            .position(|(name, context)| name == label || context.id() == label)
            .unwrap_or_else(|| panic!("未知测试连接: {label}"));
        crate::host::mcp_apps::handle_request(
            method,
            &params,
            &mut self.connections[index].1,
            Some(&facade),
        )
        .await
    }

    /// 经模型侧实例化调用签发 record（真实 bridge + 真实 peer），返回 record 身份。
    async fn instantiate(&mut self, tool: &str, arguments: Value) -> String {
        let effective = format!("mcp__{SERVER}__{tool}");
        let session = self.session.clone();
        let bridges = peri_middlewares::mcp::tool_bridge::build_tool_bridges_visible_to(
            &self.pool,
            Some(&session),
        );
        let bridge = bridges
            .iter()
            .find(|bridge| bridge.name() == effective)
            .expect("会话可见视图必须含该工具");
        let invocation_id = InvocationId::generate();
        let context = ToolContext::new(&[], ".")
            .with_session_identity(&self.session, "turn-facade")
            .with_invocation(
                Arc::new(AdmissionStub),
                invocation_id.clone(),
                InvocationOrigin::Host {
                    namespace: "test/instantiate".to_string(),
                    seq: 1,
                },
                tokio_util::sync::CancellationToken::new(),
            );
        bridge
            .invoke_output(arguments, context)
            .await
            .expect("实例化调用必须成功（真实 peer）");
        self.app_id_for(tool).await
    }

    /// 该实例化工具对应的 record 身份（经 `apps/list` 的真实投影取得）。
    async fn app_id_for(&mut self, tool: &str) -> String {
        let listed = self
            .apps("peri/mcp/apps/list", json!({"sessionId": self.session}))
            .await
            .expect("apps/list 必须成功");
        let listed: peri_acp_types::mcp_apps_facade::AppsListResponse =
            serde_json::from_value(listed).expect("apps/list 响应必须可解码");
        listed
            .descriptors
            .iter()
            .find(|descriptor| descriptor.instantiating_tool == tool)
            .unwrap_or_else(|| panic!("record 必须出现在 apps/list：{tool}"))
            .app_id
            .clone()
    }

    /// `apps/attach`（首附）。
    async fn attach(&mut self, conn: &str, app_id: &str, view: &str) -> AppsAttachResponse {
        let conn = self.connection_id(conn);
        let value = self
            .apps_from(
                &conn,
                "peri/mcp/apps/attach",
                json!({
                    "sessionId": self.session,
                    "appId": app_id,
                    "clientViewId": view,
                }),
            )
            .await
            .expect("apps/attach 必须成功");
        serde_json::from_value(value).expect("attach 响应必须可解码")
    }

    /// `apps/attach`（重挂接）。
    async fn reattach(
        &mut self,
        conn: &str,
        app_id: &str,
        view: &str,
        binding: &AttachmentBinding,
    ) -> Result<AppsAttachResponse, AcpError> {
        let conn = self.connection_id(conn);
        let value = self
            .apps_from(
                &conn,
                "peri/mcp/apps/attach",
                json!({
                    "sessionId": self.session,
                    "appId": app_id,
                    "clientViewId": view,
                    "reattach": {
                        "attachmentId": binding.attachment_id.as_str(),
                        "epoch": binding.connection_epoch.get(),
                    },
                }),
            )
            .await?;
        Ok(serde_json::from_value(value).expect("attach 响应必须可解码"))
    }

    /// `apps/request` + 同 key 有界等待终态（契约 §8：快速 `Pending`，同 key 取结果）。
    async fn request_wait(
        &mut self,
        conn: &str,
        binding: &AttachmentBinding,
        seq: u64,
        operation: Value,
    ) -> AppsRequestResponse {
        let first = self
            .request_response(conn, binding, seq, operation.clone())
            .await
            .expect("apps/request 必须被受理");
        assert!(
            first.admitted,
            "新序号必须被登记（verdict = {:?}）",
            first.verdict
        );
        assert_eq!(
            first.outcome,
            Some(AppsRequestOutcome::Pending),
            "新请求立即返回 Pending（执行在受限并发下继续）"
        );
        // 同 key 重传：加入同一等待并取得终态（不重复审批 / 执行）。
        let second = self
            .request_response(conn, binding, seq, operation)
            .await
            .expect("同 key 请求必须被受理");
        assert_eq!(
            second.verdict,
            RequestVerdict::JoinedPending,
            "同 key 必须加入同一等待"
        );
        second
    }

    async fn request_response(
        &mut self,
        conn: &str,
        binding: &AttachmentBinding,
        seq: u64,
        operation: Value,
    ) -> Result<AppsRequestResponse, AcpError> {
        let conn = self.connection_id(conn);
        let value = self
            .apps_from(
                &conn,
                "peri/mcp/apps/request",
                json!({
                    "sessionId": self.session,
                    "attachmentId": binding.attachment_id.as_str(),
                    "epoch": binding.connection_epoch.get(),
                    "seq": seq,
                    "operation": operation,
                }),
            )
            .await?;
        Ok(serde_json::from_value(value).expect("request 响应必须可解码"))
    }

    fn tools_call(name: &str, arguments: Value) -> Value {
        json!({"method": "tools/call", "name": name, "arguments": arguments})
    }

    /// `apps/detach`（幂等判决）。
    async fn detach_view(&mut self, binding: &AttachmentBinding, conn: &str) -> AppsDetachResponse {
        let conn = self.connection_id(conn);
        let value = self
            .apps_from(
                &conn,
                "peri/mcp/apps/detach",
                json!({
                    "sessionId": self.session,
                    "attachmentId": binding.attachment_id.as_str(),
                    "epoch": binding.connection_epoch.get(),
                }),
            )
            .await
            .expect("apps/detach 必须成功");
        serde_json::from_value(value).expect("detach 响应必须可解码")
    }

    /// 已送达的审批帧（本轮或历史）。
    fn permission_frames(&self) -> Vec<Value> {
        self.transport.permission_frames()
    }

    /// 有界等待 wire 调用计数到达（执行是后台任务，观测点必须等待而非假设）。
    async fn wait_for_calls(&self, expected: usize) {
        for _ in 0..400 {
            if self.calls() >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "wire 调用未在期限内到达：期望 {expected}，实际 {}",
            self.calls()
        );
    }

    /// 有界等待 service 的最新 outcome 终结（in-flight 执行的收尾观测面）。
    async fn wait_for_terminal_outcome(&self) -> SessionInvocationOutcome {
        for _ in 0..600 {
            if let Some(record) = self.service.outcomes().latest() {
                if !matches!(record.outcome, SessionInvocationOutcome::TaskAccepted(_)) {
                    return record.outcome;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("在飞执行未在期限内收尾");
    }

    /// 有界等待审批帧数量到达。
    async fn wait_for_permission_frames(&self, expected: usize) {
        for _ in 0..400 {
            if self.permission_frames().len() >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "审批帧未在期限内送达：期望 {expected}，实际 {}",
            self.permission_frames().len()
        );
    }

    /// `apps/close`（终态撤销该 App 的交互资格）。
    async fn send_close(
        &mut self,
        app_id: &str,
    ) -> peri_acp_types::mcp_apps_facade::AppsCloseResponse {
        let session = self.session.clone();
        let value = self
            .apps(
                "peri/mcp/apps/close",
                json!({"sessionId": session, "appId": app_id}),
            )
            .await
            .expect("apps/close 必须成功");
        serde_json::from_value(value).expect("close 响应必须可解码")
    }

    /// 本连接的 Host 操作域（`apps/list` 下发；`apps/rerun` 的序号权威）。
    async fn host_namespace(&mut self) -> RequestNamespaceId {
        let session = self.session.clone();
        let value = self
            .apps("peri/mcp/apps/list", json!({"sessionId": session}))
            .await
            .expect("apps/list 必须成功");
        let listed: peri_acp_types::mcp_apps_facade::AppsListResponse =
            serde_json::from_value(value).expect("apps/list 响应必须可解码");
        assert_eq!(
            listed.host_namespace.next_seq,
            RequestSeq::new(1),
            "Host 操作域的起始序号是 1"
        );
        listed.host_namespace.namespace
    }

    /// `apps/rerun`（目标来自服务端记录；同 key 复用结果）。
    async fn rerun(
        &mut self,
        namespace: &RequestNamespaceId,
        seq: u64,
        source: &InvocationId,
        arguments: Value,
    ) -> Result<AppsRequestResponse, AcpError> {
        let session = self.session.clone();
        let value = self
            .apps(
                "peri/mcp/apps/rerun",
                json!({
                    "sessionId": session,
                    "hostNamespace": namespace.as_str(),
                    "seq": seq,
                    "sourceInvocationRef": source.as_str(),
                    "arguments": arguments,
                }),
            )
            .await?;
        Ok(serde_json::from_value(value).expect("rerun 响应必须可解码"))
    }

    /// `apps/cancel`（只命中该次操作）。
    async fn cancel(&mut self, binding: &AttachmentBinding, seq: u64) -> CancelVerdict {
        let session = self.session.clone();
        let value = self
            .apps(
                "peri/mcp/apps/cancel",
                json!({
                    "sessionId": session,
                    "attachmentId": binding.attachment_id.as_str(),
                    "epoch": binding.connection_epoch.get(),
                    "seq": seq,
                }),
            )
            .await
            .expect("apps/cancel 必须成功");
        serde_json::from_value::<peri_acp_types::mcp_apps_facade::AppsCancelResponse>(value)
            .expect("cancel 响应必须可解码")
            .verdict
    }

    /// 有界等待该 key 的终态（测试观察面；生产等待走同 key 重传）。
    async fn wait_slot(&self, binding: &AttachmentBinding, seq: u64) -> Option<AppsRequestOutcome> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(slot) = self
                .facade
                .ledger()
                .slot(&binding.namespace, RequestSeq::new(seq))
            {
                return slot.wait(std::time::Duration::from_secs(5)).await;
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn completed_text(&self, outcome: &AppsRequestOutcome) -> String {
        match outcome {
            AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::Completed(raw)) => raw
                .content()
                .first()
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            other => panic!("期望 Completed 结果事实，实际: {other:?}"),
        }
    }

    async fn shutdown(self) {
        self.relay.close_session(&self.session);
        self.pool.remove_client_for_tests(SERVER);
        self.peer.close().await;
    }
}

fn assert_kind(error: &AcpError, expected: McpAppsErrorKind, context: &str) {
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("kind")),
        Some(&json!(expected)),
        "{context}，实际: {error:?}"
    );
}

// 用例容量拆分（STD-SIZE-001）：夹具留在本文件，用例分两个兄弟模块。
#[path = "apps_facade_attach_test.rs"]
mod attach_cases;
#[path = "apps_facade_cases_test.rs"]
mod cases;
