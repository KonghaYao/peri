//! [A12 / S5 收口] idle App 回调与 Host invoke 的 **canonical 入口真实路径**回归
//! （用例集见 `mcp_apps_idle_cases_test.rs`，同一模块树）。
//!
//! 真实对象：进程内 MCP peer（计数每次 `tools/call`）+ 真实连接池 + 真实 bridge/
//! canonical 管线 + 真实 record 注册表 + 真实 `SessionInvocationService` + 真实
//! `SessionInteractionRouter` + 真实 relay。
//!
//! 被替代的只有两类不可控外部输入：
//! 1. ACP 客户端帧（stub transport 记录 `session/request_permission` 并按键应答）；
//! 2. 模型侧派发器（只为签发 record 的实例化调用提供白名单解析）。
//!
//! App 回调的驱动面（S8）：旧 relay 的 `call_tool` 已删除，生产入口是
//! `SessionAppsFacade::request` → `SessionInvocationPort`；本夹具直接驱动**同一**
//! canonical 入口（少一层 wire 投影），断言落在 `SessionInvocationOutcome` 与其
//! `error_kind()` 上：审批不可用必须与「用户拒绝」可区分，且绝不自动批准。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex as StdMutex,
};

use async_trait::async_trait;
use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppInvokeRequest, McpAppsErrorKind, McpAppsRelayError, McpAppsRelayPort,
    NotExecutedCause, SessionInvocationOutcome, SessionInvocationPort, SessionInvocationRequest,
    MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use peri_acp_types::mcp_apps_facade::{
    AppAttachRequest, AttachmentBinding, ClientViewId, RequestSeq,
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
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ServerCapabilities,
    ServerConfig,
};
use serde_json::{json, Value};

use super::interaction_router::SessionInteractionRouter;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

pub(super) const SESSION: &str = "idle-acp-session";
pub(super) const SERVER: &str = "cursor-canvas";
pub(super) const APP_TOOL: &str = "show_canvas";
pub(super) const RESOURCE_URI: &str = "ui://app";

// ─── ACP 客户端帧 stub（唯一替代的传输面） ──────────────────────────────────

#[derive(Default)]
pub(super) struct FrameTransport {
    frames: StdMutex<Vec<(String, Value)>>,
    choice: StdMutex<Option<String>>,
}

impl FrameTransport {
    pub(super) fn with_choice(choice: &str) -> Arc<Self> {
        let transport = Arc::new(Self::default());
        *transport.choice.lock().unwrap() = Some(choice.to_string());
        transport
    }

    pub(super) fn permission_frames(&self) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "session/request_permission")
            .map(|(_, params)| params.clone())
            .collect()
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
    /// `Some`：第 `gate_on_call` 次调用阻塞到 gate 释放（观察 in-flight 窗口）。
    gate: Option<Arc<tokio::sync::Notify>>,
    /// 阻塞哪一次调用（默认 2：第 1 次是实例化，第 2 次才是 App 回调）。
    gate_on_call: usize,
}

impl rmcp::ServerHandler for CanvasServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new("peri-canvas", "idle_test"))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.gate_on_call {
            if let Some(gate) = self.gate.as_ref() {
                gate.notified().await;
            }
        }
        Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text("canvas ready"),
        ])))
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
            rmcp::model::Implementation::new("peri-acp-idle-test", "0.1"),
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

fn app_tool() -> rmcp::model::Tool {
    serde_json::from_value(json!({
        "name": APP_TOOL,
        "description": "canvas",
        "inputSchema": {"type": "object"},
        "_meta": {"ui": {"resourceUri": RESOURCE_URI, "visibility": ["app", "model"]}}
    }))
    .expect("工具声明必须可解析")
}

fn server_handle(peer: &Peer) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(peer.client.peer().clone()),
        tools: vec![app_tool()],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::None,
        source: None,
        url: None,
        skills_capable: false,
    })
}

/// 模型侧派发器桩：只回答「该 server 的 wire 名对应的 effective 名」。
///
/// 实例化调用（签发 record）需要白名单解析；本测试不在模型面执行工具，
/// `dispatch` 因此不可达。
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

/// 一个 View（= 一个附件）的完整身份：App 回调的全部归属事实。
#[derive(Clone)]
pub(super) struct ViewHandle {
    pub(super) app_id: String,
    pub(super) attachment_id: AttachmentId,
    pub(super) epoch: ConnectionEpoch,
    pub(super) server_generation: u64,
}

impl ViewHandle {
    /// 同一附件换新纪元（重挂接后的新句柄）。
    pub(super) fn with_epoch(&self, epoch: ConnectionEpoch) -> Self {
        Self {
            app_id: self.app_id.clone(),
            attachment_id: self.attachment_id.clone(),
            epoch,
            server_generation: self.server_generation,
        }
    }
}

pub(super) struct Fixture {
    pub(super) pool: Arc<McpClientPool>,
    pub(super) service: Arc<SessionInvocationService>,
    pub(super) relay: Arc<dyn McpAppsRelayPort>,
    pub(super) router: Arc<SessionInteractionRouter>,
    peer: Peer,
    calls: Arc<AtomicUsize>,
    /// 测试侧句柄：与 service 收到的 `session_cancel` 同一取消树（同一 session 事实）。
    ///
    /// 用它区分「会话已终结」与「record 被撤销」两条拒绝路径：直接 `cancel()` 只终结
    /// 会话，不撤销 record。
    session_cancel: tokio_util::sync::CancellationToken,
}

impl Fixture {
    pub(super) async fn new(transport: Arc<FrameTransport>) -> Self {
        Self::with_connections(transport, None, 1).await
    }

    /// `connections`：注册到 router 的在线连接数（`conn-1`、`conn-2`、…）。
    pub(super) async fn with_connections(
        transport: Arc<FrameTransport>,
        gate: Option<Arc<tokio::sync::Notify>>,
        connections: usize,
    ) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let peer = Peer::connect(CanvasServer {
            calls: Arc::clone(&calls),
            gate,
            gate_on_call: 2,
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty_for_tests());
        pool.install_client_for_tests(server_handle(&peer));
        let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
        pool.bind_session_task_manager(SESSION, &manager);

        // session-owned 交互路由：门是 session 级的（与 router 借用同一把）。
        let router = Arc::new(SessionInteractionRouter::new(
            SESSION,
            Arc::new(tokio::sync::Mutex::new(())),
        ));
        for index in 1..=connections {
            let erased: Arc<dyn AcpTransport> = Arc::clone(&transport) as Arc<dyn AcpTransport>;
            router
                .register_connection(&format!("conn-{index}"), erased)
                .expect("测试夹具连接必须可注册");
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
            session_id: SESSION.to_string(),
            cwd: Arc::from("/tmp/idle-acp"),
            queue: Arc::new(peri_acp_types::session::MessageQueue::new),
            rules: Arc::new(InvocationRuleChain::from_derived_blueprint(rules)),
            closed_instances: Default::default(),
            session_cancel: session_cancel.clone(),
        }));
        let apps_relay = PoolMcpAppsRelay::new(
            Arc::clone(&pool),
            service.runtime_epoch(),
            Arc::clone(&service) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
        );
        // 与生产装配同序：会话环境登记运行时世代（签发点据此盖章；未登记则
        // 签发点显式不签发）。
        apps_relay.register_session_incarnation(SESSION);
        let relay: Arc<dyn McpAppsRelayPort> = Arc::new(apps_relay);
        Self {
            pool,
            service,
            relay,
            router,
            peer,
            calls,
            session_cancel,
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// 经**模型侧实例化调用**签发 record，再把首个 View 附着到 `connection_id`
    /// （两条都是真实路径：真实 bridge + 真实 router + 真实 relay）。
    ///
    /// 装配顺序与生产 `apps/attach` 一致：router 分配附件身份 / namespace / 纪元
    /// （权威），relay 只按调用方给的事实附着 record（本层不铸造纪元）。
    pub(super) async fn instantiate(&self, connection_id: &str) -> ViewHandle {
        let effective = format!("mcp__{SERVER}__{APP_TOOL}");
        let bridges = peri_middlewares::mcp::tool_bridge::build_tool_bridges_visible_to(
            &self.pool,
            Some(SESSION),
        );
        let bridge = bridges
            .iter()
            .find(|bridge| bridge.name() == effective)
            .expect("会话可见视图必须含 App 工具");
        let invocation_id = InvocationId::generate();
        let context = ToolContext::new(&[], ".")
            .with_session_identity(SESSION, "turn-test")
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
            .invoke_output(json!({}), context)
            .await
            .expect("实例化调用必须成功（真实 peer）");

        // record 身份经 `apps/list` 的 descriptor 投影取得（与签发点的
        // `model.invocation_id` 同源匹配，不猜身份）。
        let app_id = self
            .relay
            .list_apps(SESSION)
            .await
            .expect("列表必须可用")
            .into_iter()
            .find(|descriptor| descriptor.model.invocation_id.as_str() == invocation_id.to_string())
            .expect("签发点必须发布该 record 的 descriptor")
            .app_id;
        self.attach_view(&app_id, connection_id, ClientViewId::new(connection_id))
            .await
    }

    /// 给**已有 record** 再挂一个 View（真实 router 分配 + 真实 `attach_app`）。
    pub(super) async fn attach_view(
        &self,
        app_id: &str,
        connection_id: &str,
        view_key: ClientViewId,
    ) -> ViewHandle {
        let attached = self
            .router
            .attach_view(super::interaction_router::ViewAttachRequest {
                attachment_id: AttachmentId::new(uuid::Uuid::now_v7().to_string()),
                app_id: app_id.to_string(),
                view_key,
                connection_id: connection_id.to_string(),
            })
            .expect("在线连接的 View 必须可附着");
        assert!(
            attached.is_deliverable(),
            "在线连接的 View 必须处于可投递状态"
        );
        self.relay
            .attach_app(AppAttachRequest {
                session_id: SESSION.to_string(),
                app_id: app_id.to_string(),
                owner_connection_id: connection_id.to_string(),
                attachment: AttachmentBinding {
                    attachment_id: attached.attachment_id().clone(),
                    namespace: attached.namespace().clone(),
                    connection_epoch: attached.epoch(),
                    next_seq: RequestSeq::new(1),
                },
                client_view_id: attached.view_key().clone(),
            })
            .await
            .expect("夹具自检：已有 record 必须可再挂 View");
        ViewHandle {
            app_id: app_id.to_string(),
            attachment_id: attached.attachment_id().clone(),
            epoch: attached.epoch(),
            server_generation: self
                .relay
                .list_apps(SESSION)
                .await
                .expect("列表必须可用")
                .into_iter()
                .find(|descriptor| descriptor.app_id == app_id)
                .expect("record 必须可读")
                .server_generation,
        }
    }

    /// 重挂接一个已 detach 的 View 到另一条在线连接：**纪元递增**（router 权威）。
    pub(super) async fn reattach_view(&self, view: &ViewHandle, connection_id: &str) -> ViewHandle {
        let next = self
            .router
            .reattach_view(&view.attachment_id, connection_id)
            .expect("窗口内必须可重挂接");
        assert_eq!(
            next.epoch(),
            view.epoch.next(),
            "重挂接必须递增纪元（旧纪元不得复用）"
        );
        self.relay
            .attach_app(AppAttachRequest {
                session_id: SESSION.to_string(),
                app_id: view.app_id.clone(),
                owner_connection_id: connection_id.to_string(),
                attachment: AttachmentBinding {
                    attachment_id: next.attachment_id().clone(),
                    namespace: next.namespace().clone(),
                    connection_epoch: next.epoch(),
                    next_seq: RequestSeq::new(1),
                },
                client_view_id: next.view_key().clone(),
            })
            .await
            .expect("重挂接必须能按新纪元替换 record 侧附件");
        view.with_epoch(next.epoch())
    }

    /// App 回调：经 **canonical 入口**（生产上由 `apps/request` façade 调用同一入口）
    /// → 审批（真实 router 选择 endpoint）→ 真实执行。
    ///
    /// 旧 relay `call_tool` 已随 S8 删除；错误面的 kind 由
    /// `SessionInvocationOutcome::error_kind`（契约层单一权威，façade 同一份投影）
    /// 得到，因此「未获批准 / 用户拒绝 / 已撤销」仍然按键可区分。
    pub(super) async fn app_callback(
        &self,
        view: &ViewHandle,
    ) -> Result<peri_acp_types::tool_outcome::RawToolResult, McpAppsErrorKind> {
        let outcome = self
            .service
            .invoke(SessionInvocationRequest {
                session_id: SESSION.to_string(),
                runtime_epoch: self.service.runtime_epoch(),
                origin: InvocationOrigin::App {
                    app_id: view.app_id.clone(),
                    attachment_id: Some(view.attachment_id.clone()),
                    connection_epoch: Some(view.epoch),
                    server_generation: view.server_generation,
                },
                target: InvocationTarget::App {
                    record_id: view.app_id.clone(),
                    tool_name: APP_TOOL.to_string(),
                    arguments: serde_json::Map::from_iter([("value".into(), json!(1))]),
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

    /// Host invoke：经 relay 的 `apps_invoke` 前置校验 + canonical 入口。
    pub(super) async fn host_invoke(&self) -> Result<Value, McpAppsRelayError> {
        let result = self
            .relay
            .invoke_app(
                &McpAppInvokeRequest {
                    envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
                    apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
                    server_id: SERVER.to_string(),
                    tool_name: APP_TOOL.to_string(),
                    owner_session_id: SESSION.to_string(),
                    arguments: serde_json::Map::new(),
                },
                // 归属值：生产上由 facade 的 Host 操作域 + 请求账本给出；本用例
                // 只驱动 relay 面，故显式给出一对固定值（`{namespace, seq}` 语义
                // 与 `TrustedInteractionOwner::Host` 同族）。
                peri_acp_types::mcp_apps::HostInvocationRef {
                    namespace: "peri/mcp/invoke".to_string(),
                    seq: 1,
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await?;
        Ok(json!({
            "toolCallId": result.tool_call_id,
            "serverId": SERVER,
            "effectiveToolName": result.effective_tool_name,
        }))
    }

    pub(super) fn latest_outcome(&self) -> SessionInvocationOutcome {
        self.service
            .outcomes()
            .latest()
            .expect("每次调用都必须写入 outcome 记录")
            .outcome
    }

    pub(super) fn cancel_session(&self) {
        self.session_cancel.cancel();
    }

    pub(super) async fn shutdown(self) {
        self.relay.close_session(SESSION);
        self.pool.remove_client_for_tests(SERVER);
        self.peer.close().await;
    }
}

pub(super) fn assert_kind(kind: McpAppsErrorKind, expected: McpAppsErrorKind, context: &str) {
    assert_eq!(kind, expected, "{context}");
}

// 用例容量拆分（STD-SIZE-001）：夹具留在本文件，用例在兄弟模块。
#[path = "mcp_apps_idle_cases_test.rs"]
mod cases;
