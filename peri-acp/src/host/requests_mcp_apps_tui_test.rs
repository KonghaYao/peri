//! 进程内 ACP host 的 MCP Apps 本地 opt-in：headless 端到端证据。
//!
//! 三种部署形态全部走**真实 host 路径**（不涉及 TUI 渲染，也不涉及浏览器）：
//! `run_acp_server_with_sessions` → `run_acp_server_inner`（连接准入由
//! `AcpServerConfig::mcp_apps_connection_available` 计算）→ server loop →
//! `session/new` 的真实会话环境装配（facade + relay + MCP 池）→ 进程内 stub MCP
//! server（rmcp duplex peer，暴露带 `_meta.ui.resourceUri` 的工具 + `ui://` 资源）。
//!
//! 覆盖三道闸：
//! 1. Apps profile（`workspace_assembly.mcp_profile`）；
//! 2. 连接准入（`mcp_apps_connection_enabled`，**不**借道 `stdio_command_filter`）；
//! 3. session entitlement（本地授权层；未注入 ⇒ `EntitlementDenied`）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::mcp_apps::MCP_APPS_HTML_MIME;
use peri_acp_types::mcp_apps_facade::{
    AppsAttachResponse, AppsListResponse, AppsRequestOutcome, AppsRequestResponse,
};
use serde_json::{json, Value};

use crate::host::assemble::HostAssemblyInput;
use crate::host::{AcpServerConfig, SharedSessions};
use crate::provider::LlmProvider;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

use super::make_peri_config_with_provider;
use super::make_provider_config;
use super::make_server_config;

const SERVER: &str = "local-apps";
const TOOL: &str = "show_canvas";
const RESOURCE_URI: &str = "ui://local-apps/canvas";
const HTML: &str = "<!doctype html><html><body><h1>peri app</h1></body></html>";

// ─── 进程内 stub MCP server（工具 + ui:// 资源）─────────────────────────────

struct StubAppServer {
    tool_calls: Arc<AtomicUsize>,
    resource_reads: Arc<AtomicUsize>,
}

impl rmcp::ServerHandler for StubAppServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(SERVER, "local_apps_test"))
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        assert_eq!(request.name.as_ref(), TOOL, "stub 只声明一个 App 工具");
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        let result: rmcp::model::CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "canvas ready"}]
        }))
        .expect("夹具结果必须可解析");
        Ok(rmcp::model::CallToolResponse::Complete(result))
    }

    async fn list_resources(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListResourcesResult {
            resources: vec![serde_json::from_value(json!({
                "uri": RESOURCE_URI,
                "name": "canvas",
                "mimeType": MCP_APPS_HTML_MIME
            }))
            .expect("夹具资源声明必须可解析")],
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        self.resource_reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.uri, RESOURCE_URI);
        let contents = rmcp::model::ResourceContents::TextResourceContents {
            uri: request.uri,
            mime_type: Some(MCP_APPS_HTML_MIME.to_string()),
            text: HTML.to_string(),
            meta: None,
        };
        Ok(rmcp::model::ReadResourceResult::new(vec![contents]).into())
    }
}

struct StubPeer {
    client: rmcp::service::RunningService<
        rmcp::service::RoleClient,
        rmcp::model::InitializeRequestParams,
    >,
    server: tokio::task::JoinHandle<()>,
}

impl StubPeer {
    async fn close(self) {
        let _ = self.client.cancel().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.server).await;
    }
}

/// 把进程内 peer 装进**会话装配产物的池**，返回调用计数与清理句柄。
async fn install_peer(
    pool: &Arc<peri_middlewares::mcp::McpClientPool>,
) -> (Arc<AtomicUsize>, Arc<AtomicUsize>, StubPeer) {
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let resource_reads = Arc::new(AtomicUsize::new(0));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server_tool_calls = Arc::clone(&tool_calls);
    let server_resource_reads = Arc::clone(&resource_reads);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        StubAppServer {
            tool_calls: server_tool_calls,
            resource_reads: server_resource_reads,
        }
        .serve(server_io)
        .await
        .unwrap()
        .waiting()
        .await
        .unwrap();
    });
    let client_info = rmcp::model::InitializeRequestParams::new(
        rmcp::model::ClientCapabilities::default(),
        rmcp::model::Implementation::new("peri-acp-local-apps-test", "0.1"),
    );
    use rmcp::ServiceExt;
    let client = tokio::time::timeout(Duration::from_secs(2), client_info.serve(client_io))
        .await
        .expect("peer 握手超时")
        .expect("peer 握手失败");
    let tool: rmcp::model::Tool = serde_json::from_value(json!({
        "name": TOOL,
        "description": "canvas",
        "inputSchema": {"type": "object", "properties": {"value": {"type": "number"}}},
        "_meta": {"ui": {"resourceUri": RESOURCE_URI, "visibility": ["app", "model"]}}
    }))
    .expect("工具声明必须可解析");
    pool.install_client_for_tests(Arc::new(peri_middlewares::mcp::client::McpClientHandle {
        name: SERVER.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(client.peer().clone()),
        tools: vec![tool],
        resources: vec![],
        status: peri_middlewares::mcp::client::ClientStatus::Connected,
        oauth_status: peri_middlewares::mcp::client::OAuthStatus::None,
        source: None,
        url: None,
        skills_capable: false,
        call_gate: Default::default(),
    }));
    (
        tool_calls,
        resource_reads,
        StubPeer {
            client,
            server: server_task,
        },
    )
}

/// 模型侧派发器桩：只回答「该 server 的 wire 名对应的 effective 名」。
struct AdmissionStub;

#[async_trait::async_trait]
impl peri_acp_types::tools::EffectiveToolDispatcher for AdmissionStub {
    async fn dispatch(
        &self,
        _: peri_acp_types::tool_invocation::EffectiveToolCall,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<peri_acp_types::tools::ToolOutput, peri_acp_types::tools::EffectiveToolError> {
        unreachable!("本用例不在模型面执行")
    }

    fn tools(&self) -> Vec<peri_acp_types::tools::EffectiveToolDefinition> {
        Vec::new()
    }

    fn admitted_mcp_tool_name(&self, server: &str, wire_name: &str) -> Option<String> {
        (server == SERVER).then(|| format!("mcp__{server}__{wire_name}"))
    }
}

/// 经真实 bridge 执行一次实例化调用 ⇒ 签发 App record（与模型执行路径同一入口）。
async fn instantiate_app_tool(pool: &Arc<peri_middlewares::mcp::McpClientPool>, session_id: &str) {
    let bridges =
        peri_middlewares::mcp::tool_bridge::build_tool_bridges_visible_to(pool, Some(session_id));
    let bridge = bridges
        .iter()
        .find(|bridge| bridge.name() == format!("mcp__{SERVER}__{TOOL}"))
        .expect("会话可见视图必须含 App 工具");
    let invocation_id = peri_acp_types::tool_invocation::InvocationId::generate();
    let context = peri_acp_types::tools::ToolContext::new(&[], ".")
        .with_session_identity(session_id, "turn-local-apps")
        .with_invocation(
            Arc::new(AdmissionStub),
            invocation_id,
            peri_acp_types::tool_invocation::InvocationOrigin::Host {
                namespace: "test/local-apps".to_string(),
                seq: 1,
            },
            tokio_util::sync::CancellationToken::new(),
        );
    bridge
        .invoke_output(json!({"value": 1}), context)
        .await
        .expect("实例化调用必须成功");
}

// ─── 进程内 ACP 客户端（真实 server loop 的 wire 驱动）──────────────────────

struct WireClient {
    incoming: tokio::sync::mpsc::UnboundedSender<IncomingMessage>,
    responses: tokio::sync::Mutex<
        tokio::sync::mpsc::UnboundedReceiver<(RequestId, Result<Value, AcpError>)>,
    >,
    next_id: std::sync::atomic::AtomicI64,
}

impl WireClient {
    fn pair() -> (Arc<WireServerTransport>, Arc<Self>) {
        let (incoming_tx, incoming_rx) = tokio::sync::mpsc::unbounded_channel();
        let (responses_tx, responses_rx) = tokio::sync::mpsc::unbounded_channel();
        let client = Arc::new(Self {
            incoming: incoming_tx,
            responses: tokio::sync::Mutex::new(responses_rx),
            next_id: std::sync::atomic::AtomicI64::new(0),
        });
        let transport = Arc::new(WireServerTransport {
            incoming: tokio::sync::Mutex::new(incoming_rx),
            responses: responses_tx,
        });
        (transport, client)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::SeqCst) + 1);
        self.incoming
            .send(IncomingMessage::Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            })
            .expect("host transport 必须仍在接收");
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (response_id, result) = self
                    .responses
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("host transport 已关闭");
                if response_id == id {
                    return result;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("ACP 请求 {method} 超时"))
    }

    async fn call_ok(&self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .await
            .unwrap_or_else(|error| panic!("{method} 必须成功: {error:?}"))
    }
}

struct WireServerTransport {
    incoming: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<IncomingMessage>>,
    responses: tokio::sync::mpsc::UnboundedSender<(RequestId, Result<Value, AcpError>)>,
}

#[async_trait::async_trait]
impl AcpTransport for WireServerTransport {
    async fn send_request(&self, method: &str, _params: Value) -> Result<Value, AcpError> {
        // 测试配置为 Bypass：正常不会走到审批。保留可答形态以免误判为挂死。
        if method == "session/request_permission" {
            return Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}));
        }
        Ok(Value::Null)
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming.lock().await.recv().await
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        let _ = self.responses.send((id, result));
        Ok(())
    }
}

// ─── 部署夹具 ───────────────────────────────────────────────────────────────

/// 三种部署形态（对应生产默认 / opt-in / 只开前两道闸）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalAppsDeployment {
    /// TUI 默认：三道闸全关。
    Disabled,
    /// opt-in：profile + 连接准入 + 本地授权层。
    Enabled,
    /// 前两道闸打开、未注入授权层（第三道闸默认拒绝）。
    EnabledWithoutAuthorizer,
}

async fn deployment_config(
    tmp: &tempfile::TempDir,
    cwd: &str,
    deployment: LocalAppsDeployment,
) -> AcpServerConfig {
    let mut peri_config = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "not-a-credential",
        "test-model",
    ));
    // 与生产装配同源：MCP 链槽开启（App 工具面 / canonical 入口需要）。
    peri_config.config.meta_harness = Some(HashMap::from([("McpMiddleware".into(), true)]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, tmp).await;
    let apps_enabled = deployment != LocalAppsDeployment::Disabled;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::deployment_profile(apps_enabled),
        capabilities: Default::default(),
    });
    cfg.mcp_apps_connection_enabled = apps_enabled;
    if deployment == LocalAppsDeployment::Enabled {
        cfg.session_authorization = Some(crate::host::assemble::local_in_process_authorizer());
    }
    cfg
}

fn workspace_fixture() -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("workspace");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir_all(target.join(".peri")).unwrap();
    (tmp, target.to_string_lossy().into_owned())
}

/// 启动真实 host、完成 initialize + session/new，返回客户端与 session id。
async fn start_host(
    cfg: AcpServerConfig,
    cwd: &str,
) -> (
    Arc<WireClient>,
    SharedSessions,
    String,
    tokio::task::JoinHandle<()>,
) {
    let (transport, client) = WireClient::pair();
    let transport: Arc<dyn AcpTransport> = transport;
    let sessions: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let task = tokio::spawn(crate::host::run_acp_server_with_sessions(
        transport,
        cfg,
        Arc::clone(&sessions),
    ));
    client
        .call("initialize", json!({"protocolVersion": 1}))
        .await
        .expect("initialize 必须成功");
    let created = client
        .call("session/new", json!({"cwd": cwd}))
        .await
        .expect("session/new 必须成功");
    let session_id = created["sessionId"]
        .as_str()
        .expect("session/new 响应必须含 sessionId")
        .to_string();
    (client, sessions, session_id, task)
}

/// 关闭 host：释放客户端（incoming 发送端）让 server loop 收到 EOF，再等真实收尾。
async fn stop_host(client: Arc<WireClient>, task: tokio::task::JoinHandle<()>) {
    drop(client);
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .expect("host 未在期限内退出")
        .expect("host task 不应 panic");
}

/// 会话环境的 MCP 池（会话装配产物的同一实例；测试在其上装入进程内 peer）。
async fn session_pool(
    sessions: &SharedSessions,
    session_id: &str,
) -> Arc<peri_middlewares::mcp::McpClientPool> {
    let environment = {
        let sessions = sessions.lock().await;
        Arc::clone(
            sessions[session_id]
                .environment
                .as_ref()
                .expect("session/new 必须装配会话环境"),
        )
    };
    let port = environment
        .cfg
        .mcp_pool
        .clone()
        .expect("会话装配必须构造 MCP 池");
    port.downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .unwrap_or_else(|_| panic!("池实现为 McpClientPool"))
}

/// `apps/request` 的有界轮询：首次 `Accepted + Pending`，同 `{namespace, seq}`
/// 重试取得同一终态（`JoinedPending` ≤30s 或 `Duplicate`）。
async fn apps_request(
    client: &WireClient,
    session_id: &str,
    binding: &peri_acp_types::mcp_apps_facade::AttachmentBinding,
    seq: peri_acp_types::mcp_apps_facade::RequestSeq,
    operation: Value,
) -> AppsRequestResponse {
    let params = json!({
        "sessionId": session_id,
        "attachmentId": binding.attachment_id.as_str(),
        "epoch": binding.connection_epoch.get(),
        "seq": seq.get(),
        "operation": operation,
    });
    for _ in 0..20 {
        let raw = client
            .call_ok("peri/mcp/apps/request", params.clone())
            .await;
        let response: AppsRequestResponse =
            serde_json::from_value(raw).expect("apps/request 响应必须可解码");
        match &response.outcome {
            Some(AppsRequestOutcome::Pending) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Some(_) => return response,
            None => panic!("未登记的请求不得给出终态: {:?}", response.verdict),
        }
    }
    panic!("apps/request 未在期限内产出终态");
}

fn assert_error_kind(error: &AcpError, expected: &str) {
    let kind = error
        .data
        .as_ref()
        .and_then(|data| data.get("kind"))
        .and_then(Value::as_str);
    assert_eq!(kind, Some(expected), "错误 kind 不符: {error:?}");
}

// ─── 用例 ───────────────────────────────────────────────────────────────────

/// 开关打开：`apps/list` → `apps/attach`（快照）→ `resources/read`（HTML）→
/// `tools/call`（App 回调）全链可用，且 attach 不调用业务工具。
#[tokio::test]
#[serial_test::serial]
async fn local_switch_on_exposes_app_snapshot_and_ui_resource() {
    let (tmp, cwd) = workspace_fixture();
    let cfg = deployment_config(&tmp, &cwd, LocalAppsDeployment::Enabled).await;
    let (client, sessions, session_id, task) = start_host(cfg, &cwd).await;

    let pool = session_pool(&sessions, &session_id).await;
    let (tool_calls, resource_reads, peer) = install_peer(&pool).await;
    instantiate_app_tool(&pool, &session_id).await;
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "实例化调用恰好一次");

    // ── apps/list：真实 host 分发 → 真实 facade / ledger / relay ──
    let listed_raw = client
        .call_ok(
            "peri/mcp/apps/list",
            json!({"sessionId": session_id.clone()}),
        )
        .await;
    let listed: AppsListResponse =
        serde_json::from_value(listed_raw).expect("apps/list 响应必须可解码");
    let descriptor = listed
        .descriptors
        .iter()
        .find(|descriptor| descriptor.instantiating_tool == TOOL)
        .expect("App record 必须出现在 apps/list");
    assert_eq!(descriptor.resource_uri, RESOURCE_URI);
    let app_id = descriptor.app_id.clone();

    // ── apps/attach：快照（uiResource 是资源**引用**，HTML 经 resources/read 取得）──
    let attached_raw = client
        .call_ok(
            "peri/mcp/apps/attach",
            json!({
                "sessionId": session_id.clone(),
                "appId": app_id,
                "clientViewId": "view-local-apps",
            }),
        )
        .await;
    let attached: AppsAttachResponse =
        serde_json::from_value(attached_raw).expect("apps/attach 响应必须可解码");
    let snapshot = &attached.snapshot;
    assert_eq!(
        snapshot.ui_resource.uri, RESOURCE_URI,
        "快照必须引用 record 的 ui:// 资源"
    );
    assert_eq!(snapshot.ui_resource.mime_type, MCP_APPS_HTML_MIME);
    assert_eq!(
        snapshot.initial_input.get("value"),
        Some(&json!(1)),
        "initialInput 是审批后的 effective 参数"
    );
    assert_eq!(
        snapshot.initial_result.as_value()["content"][0]["text"],
        json!("canvas ready"),
        "initialResult 是完整 raw 结果"
    );
    assert!(snapshot.features.tools_call && snapshot.features.resources_read);
    assert!(!snapshot.reattached, "首次附着不是重挂接");
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        1,
        "attach 不调用业务工具"
    );

    let binding = attached.attachment.clone();

    // ── apps/request → ResourcesRead：ui:// 资源内容（HTML）──
    let read = apps_request(
        &client,
        &session_id,
        &binding,
        binding.next_seq,
        json!({"method": "resources/read", "uri": RESOURCE_URI}),
    )
    .await;
    let resources = match read.outcome {
        Some(AppsRequestOutcome::ResourcesRead { resources }) => resources,
        other => panic!("期望 ResourcesRead，得到 {other:?}"),
    };
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].text.as_deref(), Some(HTML), "HTML 必须保真");
    assert!(resources[0].is_valid_app_resource(RESOURCE_URI));
    assert_eq!(resource_reads.load(Ordering::SeqCst), 1);

    // ── apps/request → ToolsCall：App 发起 tools/call 的宿主路径 ──
    let call = apps_request(
        &client,
        &session_id,
        &binding,
        read.next_seq,
        json!({"method": "tools/call", "name": TOOL, "arguments": {"value": 2}}),
    )
    .await;
    match call.outcome {
        Some(AppsRequestOutcome::ToolsCall(
            peri_acp_types::mcp_apps::SessionInvocationOutcome::Completed(raw),
        )) => {
            assert_eq!(raw.as_value()["content"][0]["text"], json!("canvas ready"));
        }
        other => panic!("期望 ToolsCall(Completed)，得到 {other:?}"),
    }
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        2,
        "实例化 + App 回调各一次"
    );

    peer.close().await;
    stop_host(client, task).await;
}

/// 开关关闭：仍是原来的 `CapabilityDisabled`（第二道闸保持关闭）。
#[tokio::test]
#[serial_test::serial]
async fn local_switch_off_keeps_capability_disabled() {
    let (tmp, cwd) = workspace_fixture();
    let cfg = deployment_config(&tmp, &cwd, LocalAppsDeployment::Disabled).await;
    assert!(
        !cfg.mcp_apps_connection_enabled && !cfg.stdio_command_filter,
        "默认部署：命令过滤与 Apps 连接准入都必须关闭"
    );
    let (client, _sessions, session_id, task) = start_host(cfg, &cwd).await;

    let error = client
        .call("peri/mcp/apps/list", json!({"sessionId": session_id}))
        .await
        .expect_err("关闭时必须拒绝 apps/list");
    assert_error_kind(&error, "capability_disabled");

    stop_host(client, task).await;
}

/// 前两道闸打开但未注入本地授权层：第三道闸默认拒绝（`EntitlementDenied`）。
#[tokio::test]
#[serial_test::serial]
async fn apps_profile_without_local_authorizer_keeps_entitlement_denied() {
    let (tmp, cwd) = workspace_fixture();
    let cfg = deployment_config(&tmp, &cwd, LocalAppsDeployment::EnabledWithoutAuthorizer).await;
    let (client, _sessions, session_id, task) = start_host(cfg, &cwd).await;

    // 第二道闸已打开：list 不受 entitlement 限制。
    let listed_raw = client
        .call_ok(
            "peri/mcp/apps/list",
            json!({"sessionId": session_id.clone()}),
        )
        .await;
    let listed: AppsListResponse =
        serde_json::from_value(listed_raw).expect("apps/list 响应必须可解码");
    assert!(listed.descriptors.is_empty(), "本用例不签发 record");

    // 受控方法在授权之前即拒绝（不查 record、不读资源）。
    let error = client
        .call(
            "peri/mcp/apps/attach",
            json!({
                "sessionId": session_id,
                "appId": "unknown-app",
                "clientViewId": "view-local-apps",
            }),
        )
        .await
        .expect_err("未注入授权层时 attach 必须拒绝");
    assert_error_kind(&error, "entitlement_denied");

    stop_host(client, task).await;
}

// ─── 装配面 opt-in 的静态证据 ───────────────────────────────────────────────

async fn assembly_input(tmp: &tempfile::TempDir, cwd: &str) -> HostAssemblyInput {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "test",
        "openai",
        "not-a-credential",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("assembly.db")))
            .await
            .unwrap();
    let config_source = Arc::new(
        crate::provider::ConfigSource::load_at(
            &tmp.path().join("empty-cwd"),
            tmp.path().join("assembly_config.json"),
        )
        .unwrap(),
    );
    HostAssemblyInput {
        provider,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        config_source,
        permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
            peri_acp_types::permission::PermissionMode::Bypass,
        ),
        session_resources,
        workspace_id: None,
        session_store_shutdown: None,
        cwd: cwd.to_owned(),
        bare: true,
        lazy_workspace: false,
        drive_cron_tick: false,
        builtin_assembly_mode: None,
        workspace_input: None,
        workspace_resources: None,
        workflow_input: None,
        builtin_closed: Default::default(),
        skills_face_closed: false,
        plugin_face_closed: false,
        prepared_plugins: None,
        session_mcp_servers: None,
        reserved_root_session_id: false,
    }
}

async fn shutdown_owners(cfg: &mut AcpServerConfig) {
    if let Some(mut owner) = cfg.host_task_owner.take() {
        let _ = owner.shutdown().await;
    }
    if let Some(mut owner) = cfg.mcp_task_owner.take() {
        let _ = owner.shutdown().await;
    }
}

/// 默认关闭：`false` 变体与 `assemble_server_config` 等价；`true` 变体只打开
/// Apps 的两道闸（profile/连接/授权），**不**改 `stdio_command_filter`。
#[tokio::test]
#[serial_test::serial]
async fn local_apps_assembly_is_opt_in_and_keeps_stdio_command_filter_off() {
    let (tmp, cwd) = workspace_fixture();

    let mut off = crate::host::assemble::assemble_server_config_with_local_mcp_apps(
        assembly_input(&tmp, &cwd).await,
        false,
    )
    .await;
    assert!(
        !off.stdio_command_filter && !off.mcp_apps_connection_enabled,
        "关闭时不得打开任何一道闸"
    );
    assert!(off.session_authorization.is_none(), "关闭时不得注入授权层");
    assert!(
        !off.workspace_assembly
            .as_ref()
            .expect("host 级装配必须提供 workspace assembly")
            .mcp_profile
            .apps_enabled(),
        "关闭时 profile 必须 disabled"
    );
    assert!(!off.mcp_apps_connection_available());
    shutdown_owners(&mut off).await;

    let mut on = crate::host::assemble::assemble_server_config_with_local_mcp_apps(
        assembly_input(&tmp, &cwd).await,
        true,
    )
    .await;
    assert!(
        !on.stdio_command_filter,
        "Apps opt-in 不得借道命令过滤：TUI 的 rewind/clear 语义必须保持"
    );
    assert!(on.mcp_apps_connection_enabled, "连接准入门必须打开");
    assert!(on.session_authorization.is_some(), "必须注入本地授权层");
    assert!(
        on.workspace_assembly
            .as_ref()
            .expect("host 级装配必须提供 workspace assembly")
            .mcp_profile
            .apps_enabled(),
        "profile 必须协商出 Apps MIME"
    );
    assert!(on.mcp_apps_connection_available());
    shutdown_owners(&mut on).await;
}
