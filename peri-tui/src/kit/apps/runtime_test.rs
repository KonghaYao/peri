use super::relay::RelayContext;
use super::view_task::{AppRequestCursor, ViewContext, run_view};
use super::*;
use crate::kit::apps::mapping::ViewportPoint;
use crate::kit::apps::request_queue::PendingAppRequest;
use crate::kit::apps::state::{APPS_STATE, AppEntry, AppView, ViewPhase};
use crate::kit::apps::view_client::{
    CreatedViewSession, MAX_VIEW_HTML_BYTES, ViewAppResult, ViewEvent, ViewEventSource,
    ViewSessionApi, ViewSessionStatus,
};
use async_trait::async_trait;
use base64::Engine as _;
use peri_acp_types::interaction::{AttachmentId, ConnectionEpoch};
use peri_acp_types::mcp_apps::{MCP_APPS_HTML_MIME, RawResource, SessionInvocationOutcome};
use peri_acp_types::mcp_apps_facade::{
    AppAttachSnapshot, AppDescriptor, AppExecutionState, AppQualification, AppsOperation,
    AppsRequestOutcome, AppsRequestResponse, AttachmentBinding, HostNamespace, ModelAssociation,
    RequestNamespaceId, RequestSeq, RequestVerdict, UiResourceRef,
};
use peri_acp_types::tool_invocation::{InvocationId, ToolCallId};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RESOURCE_URI: &str = "ui://get-time/mcp-app.html";
const HTML: &str = "<html><body>clock</body></html>";

// ── fixture ────────────────────────────────────────────────────────────────

fn descriptor(app_id: &str, tool_call_id: Option<&str>) -> AppDescriptor {
    AppDescriptor {
        app_id: app_id.to_string(),
        server_id: "demo".to_string(),
        server_generation: 1,
        resource_uri: RESOURCE_URI.to_string(),
        instantiating_tool: "get-time".to_string(),
        qualification: AppQualification::DisplayOnly,
        model: ModelAssociation {
            invocation_id: InvocationId::new("inv-1"),
            tool_call_id: tool_call_id.map(ToolCallId::new),
        },
        execution_state: AppExecutionState::Live,
        attachments: Vec::new(),
        retention_remaining_ms: 60_000,
    }
}

fn binding(seq: u64) -> AttachmentBinding {
    AttachmentBinding {
        attachment_id: AttachmentId::new("att-1"),
        namespace: RequestNamespaceId::new(),
        connection_epoch: ConnectionEpoch::initial(),
        next_seq: RequestSeq::new(seq),
    }
}

fn attach_response(_html: Option<&str>) -> AppsAttachResponse {
    let attachment = binding(1);
    let snapshot = AppAttachSnapshot {
        descriptor: descriptor("app-1", Some("call-1")),
        attachment: attachment.clone(),
        ui_resource: UiResourceRef {
            uri: RESOURCE_URI.to_string(),
            mime_type: MCP_APPS_HTML_MIME.to_string(),
        },
        initial_input: serde_json::Map::new(),
        initial_result: peri_acp_types::mcp_apps::RawCallToolResult::new(json!({})),
        features: peri_acp_types::mcp_apps_facade::McpAppsFeatures::default(),
        limits: peri_acp_types::mcp_apps_facade::McpAppsLimits::default(),
        reattached: false,
    };
    AppsAttachResponse {
        attachment,
        snapshot,
        features: peri_acp_types::mcp_apps_facade::McpAppsFeatures::default(),
        limits: peri_acp_types::mcp_apps_facade::McpAppsLimits::default(),
    }
}

fn list_response(descriptors: Vec<AppDescriptor>) -> AppsListResponse {
    AppsListResponse {
        descriptors,
        features: peri_acp_types::mcp_apps_facade::McpAppsFeatures::default(),
        limits: peri_acp_types::mcp_apps_facade::McpAppsLimits::default(),
        host_namespace: HostNamespace {
            namespace: RequestNamespaceId::new(),
            next_seq: RequestSeq::new(1),
        },
    }
}

fn raw_html(text: &str) -> RawResource {
    RawResource {
        uri: RESOURCE_URI.to_string(),
        mime_type: MCP_APPS_HTML_MIME.to_string(),
        text: Some(text.to_string()),
        blob: None,
        meta: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

fn read_response(resources: Vec<RawResource>, seq: u64) -> AppsRequestResponse {
    AppsRequestResponse {
        verdict: RequestVerdict::Accepted,
        admitted: true,
        outcome: Some(AppsRequestOutcome::ResourcesRead { resources }),
        next_seq: RequestSeq::new(seq + 1),
    }
}

fn pending_response(seq: u64) -> AppsRequestResponse {
    AppsRequestResponse {
        verdict: RequestVerdict::Accepted,
        admitted: true,
        outcome: Some(AppsRequestOutcome::Pending),
        next_seq: RequestSeq::new(seq + 1),
    }
}

// ── fake RPC ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct FakeRpc {
    session: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
    list: Mutex<Option<AppsListResponse>>,
    attach: Mutex<Option<Result<AppsAttachResponse, RpcError>>>,
    /// 依次消费；空时用 `read_default`。
    read_queue: Mutex<VecDeque<AppsRequestResponse>>,
    read_default: Mutex<Option<Result<AppsRequestResponse, RpcError>>>,
    read_seqs: Mutex<Vec<u64>>,
    /// tools/call 的应答脚本（依次消费）；空时用 `tool_default`。
    tool_queue: Mutex<VecDeque<Result<AppsRequestResponse, RpcError>>>,
    tool_default: Mutex<Option<Result<AppsRequestResponse, RpcError>>>,
    tool_calls: Mutex<Vec<ToolCallLog>>,
}

#[derive(Debug, Clone, PartialEq)]
struct ToolCallLog {
    attachment_id: String,
    epoch: u64,
    seq: u64,
    name: String,
    arguments: serde_json::Value,
}

impl FakeRpc {
    fn with_session(session: &str) -> Self {
        Self {
            session: Mutex::new(Some(session.to_string())),
            ..Self::default()
        }
    }

    fn set_attach_ok(&self, html: Option<&str>) {
        *self.attach.lock().unwrap() = Some(Ok(attach_response(html)));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AppsRpc for FakeRpc {
    fn current_session_id(&self) -> Option<String> {
        self.session.lock().unwrap().clone()
    }

    async fn list(&self, _session_id: &str) -> Result<AppsListResponse, RpcError> {
        self.calls.lock().unwrap().push("list".to_string());
        self.list
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| RpcError::new("fake_list_missing"))
    }

    async fn attach(&self, _request: AppsAttachRequest) -> Result<AppsAttachResponse, RpcError> {
        self.calls.lock().unwrap().push("attach".to_string());
        self.attach
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Err(RpcError::new("fake_attach_missing")))
    }

    async fn request(&self, request: AppsRequestRequest) -> Result<AppsRequestResponse, RpcError> {
        self.calls.lock().unwrap().push("request".to_string());
        match &request.operation {
            AppsOperation::ResourcesRead { uri } => {
                assert_eq!(uri, RESOURCE_URI);
                self.read_seqs.lock().unwrap().push(request.seq.get());
                let queued = self.read_queue.lock().unwrap().pop_front();
                return match queued {
                    Some(response) => Ok(response),
                    None => self
                        .read_default
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| Err(RpcError::new("fake_read_missing"))),
                };
            }
            AppsOperation::ToolsCall { name, arguments } => {
                self.tool_calls.lock().unwrap().push(ToolCallLog {
                    attachment_id: request.attachment_id.as_str().to_string(),
                    epoch: request.epoch.get(),
                    seq: request.seq.get(),
                    name: name.clone(),
                    arguments: Value::Object(arguments.clone()),
                });
                let queued = self.tool_queue.lock().unwrap().pop_front();
                return match queued {
                    Some(response) => response,
                    None => self
                        .tool_default
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| Err(RpcError::new("fake_tool_missing"))),
                };
            }
            other => panic!("runtime 发送了未预期的方法: {other:?}"),
        }
    }

    async fn detach(&self, _request: AppsDetachRequest) -> Result<(), RpcError> {
        self.calls.lock().unwrap().push("detach".to_string());
        Ok(())
    }
}

// ── fake view API ──────────────────────────────────────────────────────────

struct FakeSource {
    events: VecDeque<ViewEvent>,
    hold: bool,
    /// 测试节奏：每个事件投递前消耗一个许可（事件序列不再一次性冲进事件循环）。
    pace: Option<Arc<tokio::sync::Semaphore>>,
}

#[async_trait]
impl ViewEventSource for FakeSource {
    async fn next_event(
        &mut self,
    ) -> Result<Option<ViewEvent>, crate::kit::apps::view_client::ViewClientError> {
        if let (Some(pace), false) = (self.pace.as_ref(), self.events.is_empty()) {
            let Ok(permit) = Arc::clone(pace).acquire_owned().await else {
                return Ok(None);
            };
            permit.forget();
        }
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        if self.hold {
            peri_time::sleep(Duration::from_secs(3600)).await;
        }
        Ok(None)
    }
}

#[derive(Default)]
struct FakeViewApi {
    created: Mutex<Vec<String>>,
    created_html: Mutex<Vec<String>>,
    deleted: Mutex<Vec<String>>,
    connections: AtomicUsize,
    /// 每个连接的脚本；空时返回 hold 连接的 source。
    scripts: Mutex<VecDeque<VecDeque<ViewEvent>>>,
    hold: bool,
    pace: Option<Arc<tokio::sync::Semaphore>>,
    clicks: Mutex<Vec<(String, ViewportPoint)>>,
    app_results: Mutex<Vec<(String, u64, ViewAppResult)>>,
}

#[async_trait]
impl ViewSessionApi for FakeViewApi {
    async fn create_session(
        &self,
        html: String,
    ) -> Result<CreatedViewSession, crate::kit::apps::view_client::ViewClientError> {
        let mut created = self.created.lock().unwrap();
        let id = format!("view-{}", created.len() + 1);
        created.push(id.clone());
        self.created_html.lock().unwrap().push(html);
        Ok(CreatedViewSession {
            session_id: id.clone(),
            events_url: format!("/view/sessions/{id}/events"),
        })
    }

    async fn delete_session(
        &self,
        session_id: &str,
    ) -> Result<(), crate::kit::apps::view_client::ViewClientError> {
        self.deleted.lock().unwrap().push(session_id.to_string());
        Ok(())
    }

    async fn click(
        &self,
        session_id: &str,
        point: ViewportPoint,
    ) -> Result<(), crate::kit::apps::view_client::ViewClientError> {
        self.clicks
            .lock()
            .unwrap()
            .push((session_id.to_string(), point));
        Ok(())
    }

    async fn app_result(
        &self,
        session_id: &str,
        seq: u64,
        result: ViewAppResult,
    ) -> Result<(), crate::kit::apps::view_client::ViewClientError> {
        self.app_results
            .lock()
            .unwrap()
            .push((session_id.to_string(), seq, result));
        Ok(())
    }

    async fn connect_events(
        &self,
        _session_id: &str,
        _events_url: Option<&str>,
    ) -> Result<Box<dyn ViewEventSource>, crate::kit::apps::view_client::ViewClientError> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        let script = self.scripts.lock().unwrap().pop_front();
        Ok(Box::new(FakeSource {
            events: script.unwrap_or_default(),
            hold: self.hold,
            pace: self.pace.clone(),
        }))
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn entry(app_id: &str) -> AppEntry {
    AppEntry {
        session_id: "session-1".to_string(),
        app_id: app_id.to_string(),
        server_id: "demo".to_string(),
        resource_uri: RESOURCE_URI.to_string(),
        instantiating_tool: "get-time".to_string(),
        tool_call_id: Some("call-1".to_string()),
    }
}

fn view_context(
    view_id: &str,
    rpc: Arc<dyn AppsRpc>,
    api: Arc<dyn ViewSessionApi>,
    cancel: CancellationToken,
) -> ViewContext {
    view_context_with_decode(
        view_id,
        rpc,
        api,
        cancel,
        super::frame_decode::jpeg_decoder(),
    )
}

fn view_context_with_decode(
    view_id: &str,
    rpc: Arc<dyn AppsRpc>,
    api: Arc<dyn ViewSessionApi>,
    cancel: CancellationToken,
    decode: super::frame_decode::FrameDecodeFn,
) -> ViewContext {
    ViewContext {
        rpc,
        api,
        session_id: "session-1".to_string(),
        entry: entry("app-1"),
        view_id: view_id.to_string(),
        cancel,
        policy: StreamPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
        },
        decode,
    }
}

fn seed_view(view_id: &str) {
    APPS_STATE
        .state()
        .set(crate::kit::apps::state::AppsState::default());
    state::insert_view(AppView::new(view_id, entry("app-1")));
}

async fn wait_for_async(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        peri_time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not reached before timeout");
}

fn ready_status() -> ViewEvent {
    ViewEvent::Status(ViewSessionStatus {
        state: "ready".to_string(),
        code: None,
        frames: 0,
        pending_requests: 0,
    })
}

fn closed(reason: &str) -> ViewEvent {
    ViewEvent::Closed {
        reason: Some(reason.to_string()),
    }
}

fn frame_event(seq: u64) -> ViewEvent {
    ViewEvent::Frame(crate::kit::apps::view_client::FrameEvent {
        seq,
        width: 2,
        height: 2,
        viewport_width: 2,
        viewport_height: 2,
        data_base64: base64::engine::general_purpose::STANDARD.encode(tiny_jpeg()),
    })
}

fn app_request(seq: u64, method: &str, params: serde_json::Value) -> PendingAppRequest {
    PendingAppRequest {
        seq,
        id: json!(format!("rpc-{seq}")),
        method: method.to_string(),
        params,
    }
}

fn tools_call(seq: u64, name: &str, arguments: serde_json::Value) -> PendingAppRequest {
    app_request(
        seq,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

fn completed_tool(value: serde_json::Value) -> SessionInvocationOutcome {
    SessionInvocationOutcome::Completed(peri_acp_types::mcp_apps::RawCallToolResult::new(value))
}

fn tool_response(outcome: SessionInvocationOutcome, next_seq: u64) -> AppsRequestResponse {
    AppsRequestResponse {
        verdict: RequestVerdict::Accepted,
        admitted: true,
        outcome: Some(AppsRequestOutcome::ToolsCall(outcome)),
        next_seq: RequestSeq::new(next_seq),
    }
}

fn relay_context(
    attachment: &str,
    epoch: u64,
    view_id: &str,
    view_session_id: &str,
    rpc: Arc<dyn AppsRpc>,
    api: Arc<dyn ViewSessionApi>,
    budget: Duration,
) -> RelayContext {
    RelayContext {
        rpc,
        api,
        session_id: "session-1".to_string(),
        attachment_id: AttachmentId::new(attachment),
        epoch: ConnectionEpoch::new(epoch),
        view_id: view_id.to_string(),
        view_session_id: view_session_id.to_string(),
        cancel: CancellationToken::new(),
        budget,
        retry_interval: Duration::from_millis(5),
    }
}

fn tiny_jpeg() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
    let mut buffer = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new(&mut buffer)
        .encode_image(&image)
        .expect("encode jpeg");
    buffer.into_inner()
}

// ── tests ──────────────────────────────────────────────────────────────────

#[serial_test::serial]
#[tokio::test]
async fn attach_and_resources_read_never_call_business_tools() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi::default());
    api.scripts.lock().unwrap().push_back(VecDeque::from(vec![
        ready_status(),
        frame_event(1),
        closed("deleted"),
    ]));

    let context = view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    );
    run_view(context).await;

    let calls = rpc.calls();
    assert_eq!(
        calls,
        vec!["attach", "request", "detach"],
        "只允许 attach / resources-read request / detach，绝不调用业务工具"
    );
    assert_eq!(rpc.read_seqs.lock().unwrap().as_slice(), &[1]);
    assert!(
        rpc.tool_calls.lock().unwrap().is_empty(),
        "attach + resources/read 路径不得产生 tools/call"
    );
    assert_eq!(
        api.created_html.lock().unwrap().as_slice(),
        &[HTML.to_string()]
    );
    assert_eq!(
        api.deleted.lock().unwrap().as_slice(),
        &["view-1".to_string()]
    );

    let state = APPS_STATE.state().read().clone();
    let view = state.view("v1").expect("view");
    assert_eq!(view.attachment_id.as_deref(), Some("att-1"));
    assert_eq!(view.ui_resource_uri.as_deref(), Some(RESOURCE_URI));
    assert_eq!(view.ui_resource_mime.as_deref(), Some(MCP_APPS_HTML_MIME));
    assert!(matches!(view.phase, ViewPhase::Closed { .. }));
    assert_eq!(view.frame.as_ref().map(|frame| frame.seq), Some(1));
}

#[serial_test::serial]
#[tokio::test]
async fn attach_failure_marks_local_error_without_view_session() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.attach.lock().unwrap() = Some(Err(RpcError::new("entitlement_denied")));
    let api = Arc::new(FakeViewApi::default());
    let context = view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    );
    run_view(context).await;

    let state = APPS_STATE.state().read().clone();
    assert_eq!(
        state.view("v1").unwrap().phase,
        ViewPhase::LocalError {
            code: "entitlement_denied".to_string()
        }
    );
    assert!(api.created.lock().unwrap().is_empty());
    assert!(rpc.calls().iter().all(|call| call != "request"));
}

#[serial_test::serial]
#[tokio::test]
async fn resources_read_pending_retries_same_seq_then_succeeds() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    let mut queue = VecDeque::new();
    queue.push_back(pending_response(1));
    queue.push_back(read_response(vec![raw_html(HTML)], 1));
    *rpc.read_queue.lock().unwrap() = queue;
    let api = Arc::new(FakeViewApi::default());
    api.scripts
        .lock()
        .unwrap()
        .push_back(VecDeque::from(vec![closed("deleted")]));

    run_view(view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(
        rpc.read_seqs.lock().unwrap().as_slice(),
        &[1, 1],
        "Pending 必须用同一 seq 重试（ledger 以 key 去重）"
    );
    assert_eq!(api.created.lock().unwrap().len(), 1);
}

#[serial_test::serial]
#[tokio::test]
async fn oversized_ui_resource_is_rejected_before_creating_a_session() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    let oversized = "x".repeat(MAX_VIEW_HTML_BYTES + 1);
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(&oversized)], 1)));
    let api = Arc::new(FakeViewApi::default());

    run_view(view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    ))
    .await;

    let state = APPS_STATE.state().read().clone();
    assert_eq!(
        state.view("v1").unwrap().phase,
        ViewPhase::LocalError {
            code: "html_too_large".to_string()
        }
    );
    assert!(
        api.created.lock().unwrap().is_empty(),
        "5 MiB 上限先于建会话"
    );
}

#[serial_test::serial]
#[tokio::test]
async fn sse_disconnect_reconnects_and_keeps_frame_attribution() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi::default());
    {
        let mut scripts = api.scripts.lock().unwrap();
        // 第一次连接：ready + 一帧后断线（EOF）。
        scripts.push_back(VecDeque::from(vec![ready_status(), frame_event(1)]));
        // 第二次连接：ready + 新帧 + 远端关闭。
        scripts.push_back(VecDeque::from(vec![
            ready_status(),
            frame_event(2),
            closed("deleted"),
        ]));
    }

    run_view(view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(
        api.connections.load(Ordering::SeqCst),
        2,
        "断线后必须重新订阅同一 view session"
    );
    let state = APPS_STATE.state().read().clone();
    let view = state.view("v1").expect("view");
    assert_eq!(view.frame.as_ref().map(|frame| frame.seq), Some(2));
    assert!(matches!(view.phase, ViewPhase::Closed { .. }));
}

#[serial_test::serial]
#[tokio::test]
async fn disabled_availability_spawns_nothing_and_calls_nothing() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    let handle = spawn_with_availability(
        AppsAvailability::Disabled,
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        CancellationToken::new(),
    );
    assert!(handle.is_none());
    assert!(rpc.calls().is_empty(), "开关关闭时零 RPC");
    assert!(!runtime_installed(), "未安装命令通道");

    let handle = spawn_with_availability(
        AppsAvailability::Unconfigured {
            missing: super::super::config::VIEW_URL_ENV,
        },
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        CancellationToken::new(),
    );
    assert!(handle.is_none());
    assert!(rpc.calls().is_empty(), "缺配置时零 RPC");
}

#[serial_test::serial]
#[tokio::test]
async fn runtime_discovers_opens_two_views_and_deletes_on_shutdown() {
    APPS_STATE
        .state()
        .set(crate::kit::apps::state::AppsState::default());
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.list.lock().unwrap() = Some(list_response(vec![
        descriptor("app-1", Some("call-1")),
        descriptor("app-2", Some("call-2")),
    ]));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });

    let shutdown = CancellationToken::new();
    let handle = spawn_with_parts(
        super::super::config::AppsConfig {
            view_url: "http://127.0.0.1:1".to_string(),
            token: super::super::config::ViewToken::new("token").unwrap(),
            discovery_interval: Duration::from_millis(20),
        },
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        shutdown.clone(),
    );

    wait_for_async(|| !APPS_STATE.state().read().descriptors.is_empty()).await;
    assert_eq!(APPS_STATE.state().read().descriptors.len(), 2);
    assert!(request_open_app("app-1"));
    wait_for_async(|| api.created.lock().unwrap().len() == 1).await;
    assert!(request_open_app("app-2"));
    wait_for_async(|| api.created.lock().unwrap().len() == 2).await;
    assert_eq!(APPS_STATE.state().read().views.len(), 2, "A6：两个视图并存");
    // 同一 App 再次打开只切活动视图，不重复 attach。
    assert!(request_open_app("app-1"));
    peri_time::sleep(Duration::from_millis(30)).await;
    assert_eq!(api.created.lock().unwrap().len(), 2);

    handle.shutdown().await;
    let mut deleted = api.deleted.lock().unwrap().clone();
    deleted.sort();
    assert_eq!(deleted, vec!["view-1".to_string(), "view-2".to_string()]);
    assert!(APPS_STATE.state().read().views.is_empty(), "退出时清理视图");
    assert!(!runtime_installed());
    shutdown.cancel();
}

#[serial_test::serial]
#[tokio::test]
async fn session_change_closes_previous_views() {
    APPS_STATE
        .state()
        .set(crate::kit::apps::state::AppsState::default());
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.list.lock().unwrap() = Some(list_response(vec![descriptor("app-1", Some("call-1"))]));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });

    let handle = spawn_with_parts(
        super::super::config::AppsConfig {
            view_url: "http://127.0.0.1:1".to_string(),
            token: super::super::config::ViewToken::new("token").unwrap(),
            discovery_interval: Duration::from_millis(10),
        },
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    );
    wait_for_async(|| !APPS_STATE.state().read().descriptors.is_empty()).await;
    assert!(request_open_app("app-1"));
    wait_for_async(|| api.created.lock().unwrap().len() == 1).await;

    *rpc.session.lock().unwrap() = Some("session-2".to_string());
    wait_for_async(|| !api.deleted.lock().unwrap().is_empty()).await;
    assert!(APPS_STATE.state().read().views.is_empty());
    handle.shutdown().await;
}

#[serial_test::serial]
#[tokio::test]
async fn tool_card_entry_opens_panel_and_closing_panel_deletes_sessions() {
    APPS_STATE
        .state()
        .set(crate::kit::apps::state::AppsState::default());
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.list.lock().unwrap() = Some(list_response(vec![descriptor("app-1", Some("call-1"))]));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });
    let handle = spawn_with_parts(
        super::super::config::AppsConfig {
            view_url: "http://127.0.0.1:1".to_string(),
            token: super::super::config::ViewToken::new("token").unwrap(),
            discovery_interval: Duration::from_millis(10),
        },
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    );
    wait_for_async(|| !APPS_STATE.state().read().descriptors.is_empty()).await;

    // A1 入口：descriptor.model.toolCallId == 工具卡 tool_id → 打开并切到面板。
    assert!(
        crate::kit::apps::open_from_tool_card("session-1", "call-1"),
        "匹配到的已完成工具卡必须能打开 App"
    );
    wait_for_async(|| api.created.lock().unwrap().len() == 1).await;
    assert_eq!(
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        Some(crate::app::panel_types::PanelKind::McpApps)
    );
    assert!(
        !crate::kit::apps::open_from_tool_card("session-1", "unknown-call"),
        "无匹配时保持零副作用"
    );

    // 面板关闭 → 所有视图 DELETE（TUI 退出路径之外的第二条清理入口）。
    assert!(crate::kit::panel_registry::close_panel(
        crate::app::panel_types::PanelKind::McpApps
    ));
    wait_for_async(|| !api.deleted.lock().unwrap().is_empty()).await;
    assert_eq!(
        api.deleted.lock().unwrap().as_slice(),
        &["view-1".to_string()]
    );
    assert!(APPS_STATE.state().read().views.is_empty());

    handle.shutdown().await;
}

/// 真实网关冒烟（可选）：没有 `PERI_BROWSER_VIEW_URL`/token 或网关未起时跳过。
///
/// 不依赖 Kitesurf：只验证鉴权 + 空 HTML 校验路径，不创建页面、不订阅帧。
#[serial_test::serial]
#[tokio::test]
async fn real_gateway_auth_smoke_is_optional() {
    let (Ok(url), Ok(token)) = (
        std::env::var(super::super::config::VIEW_URL_ENV),
        std::env::var(super::super::config::VIEW_TOKEN_ENV),
    ) else {
        eprintln!("skip: PERI_BROWSER_VIEW_URL / PERI_BROWSER_VIEW_TOKEN not set");
        return;
    };
    let client = AppsViewClient::new(&url, super::super::config::ViewToken::new(&token).unwrap())
        .expect("client");
    // 空 HTML：网关在建页面前返回 400 empty_html，不触碰 Kitesurf。
    let error = client.create_session("").await.expect_err("empty html");
    assert_eq!(error.code(), "empty_html");
}

/// 帧解码闸门：`seq == 1` 的解码会卡在 [`DecodeGate::wait`]，直到测试放行。
#[derive(Default)]
struct DecodeGate {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl DecodeGate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            let (guard, timeout) = self.cv.wait_timeout(open, Duration::from_secs(5)).unwrap();
            if timeout.timed_out() {
                return;
            }
            open = guard;
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

fn stub_frame_image(seq: u64) -> crate::kit::apps::state::FrameImage {
    crate::kit::apps::state::FrameImage {
        seq,
        width: 1,
        height: 1,
        viewport_width: 1,
        viewport_height: 1,
        image: Arc::new(image::DynamicImage::new_rgb8(1, 1)),
    }
}

/// 回归：帧解码不得阻塞 SSE 事件循环。
///
/// 原缺陷：事件循环对每帧 `await` base64+JPEG 解码，帧快于解码时积压，
/// 排在流后面的 `app-request` 被推迟（真实会话实测 5–10s）。
/// 这里把首帧解码卡住：只有「投递不等解码」的实现才能让 app-request 进入
/// 中继并完成 tools/call；旧实现会在 `wait_for_async` 超时后失败。
#[serial_test::serial]
#[tokio::test]
async fn app_request_is_not_blocked_by_frame_decoding() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    *rpc.tool_default.lock().unwrap() =
        Some(Ok(tool_response(completed_tool(json!({"ok": true})), 2)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        // 先只放行 ready + 帧 1；其余事件在首帧解码卡住后再投递。
        pace: Some(Arc::new(tokio::sync::Semaphore::new(2))),
        ..FakeViewApi::default()
    });
    api.scripts.lock().unwrap().push_back(VecDeque::from(vec![
        ready_status(),
        frame_event(1),
        frame_event(2),
        frame_event(3),
        ViewEvent::AppRequest(tools_call(1, "sync", json!({}))),
    ]));
    let pace = Arc::clone(api.pace.as_ref().expect("pace"));

    let gate = Arc::new(DecodeGate::default());
    let decoded = Arc::new(Mutex::new(Vec::new()));
    let decode = {
        let gate = Arc::clone(&gate);
        let decoded = Arc::clone(&decoded);
        Arc::new(move |frame: &crate::kit::apps::view_client::FrameEvent| {
            decoded.lock().unwrap().push(frame.seq);
            if frame.seq == 1 {
                gate.wait();
            }
            Some(stub_frame_image(frame.seq))
        })
    };
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run_view(view_context_with_decode(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        cancel.clone(),
        decode,
    )));

    // 首帧解码在途（被卡住）：事件循环必须继续，帧 2/3 与 app-request 照常投递。
    wait_for_async(|| decoded.lock().unwrap().contains(&1)).await;
    assert_eq!(
        *decoded.lock().unwrap(),
        vec![1],
        "解码 1 卡住时不能有别的帧被解"
    );
    pace.add_permits(4);
    wait_for_async(|| !rpc.tool_calls.lock().unwrap().is_empty()).await;
    let call = rpc.tool_calls.lock().unwrap()[0].clone();
    assert_eq!(call.name, "sync");
    assert_eq!(
        call.seq, 2,
        "apps/request 的 ledger 序号从 resources/read 的 nextSeq 继续"
    );
    assert_eq!(
        *decoded.lock().unwrap(),
        vec![1],
        "帧 2/3 只是投递到单槽，不等待解码"
    );

    // 回填后网关收到 /app-results，pendingRequests 回落。
    wait_for_async(|| api.app_results.lock().unwrap().len() == 1).await;
    wait_for_async(|| {
        APPS_STATE
            .state()
            .read()
            .view("v1")
            .is_some_and(|view| view.pending_requests == 0)
    })
    .await;

    // 放行解码：积压的 2 被合并，只解最新帧 3。
    gate.open();
    wait_for_async(|| {
        APPS_STATE
            .state()
            .read()
            .view("v1")
            .and_then(|view| view.frame.as_ref().map(|frame| frame.seq))
            == Some(3)
    })
    .await;
    assert_eq!(
        *decoded.lock().unwrap(),
        vec![1, 3],
        "单槽只保留最新帧：中间帧被合并丢弃"
    );

    // 取消结束视图：解码任务与清理都不得挂死。
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("视图收尾不得挂死")
        .expect("join");
    assert_eq!(
        api.deleted.lock().unwrap().as_slice(),
        &["view-1".to_string()]
    );
}

#[path = "runtime/relay_test.rs"]
mod relay_tests;

#[test]
fn app_request_cursor_drops_replayed_requests() {
    let mut cursor = AppRequestCursor::default();
    assert!(cursor.accept(1));
    assert!(cursor.accept(2));
    assert!(!cursor.accept(2), "重连重放同一 seq 必须跳过");
    assert!(!cursor.accept(1), "旧 seq 不得再次入队");
    assert!(cursor.accept(3));
}
