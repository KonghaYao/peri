//! `UpstreamProxy` 的 wire-level 验收：真实 rmcp client/server 经 duplex 链路。
//!
//! 覆盖并发关联、取消、progress token、task id/owner、资源读/订阅、通知透传、
//! 断线降级与重连隔离；不以 `tools/list` 单独代替协议验收。

use std::{
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use parking_lot::Mutex;

#[allow(deprecated)]
use rmcp::model::{LoggingLevel, LoggingMessageNotificationParam};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams,
        CancelledNotificationParam, ClientCapabilities, ClientRequest, ContentBlock,
        CreateTaskResult, CustomNotification, CustomRequest, CustomResult, DetailedTask, ErrorCode,
        ExtensionCapabilities, GetMeta, GetTaskParams, GetTaskResult, Implementation,
        InitializeRequestParams, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ProgressNotificationParam, ProgressToken, PromptsCapability, ReadResourceRequestParams,
        ReadResourceResult, RequestId, RequestMetaObject, Resource, ResourceContents,
        ResourceUpdatedNotification, ResourceUpdatedNotificationParam, ServerCapabilities,
        ServerConfig, ServerNotification, SubscribeRequestParams, SubscriptionFilter, Task,
        TaskPayload, TaskStatus, TaskStatusNotification, TaskStatusNotificationParams, Tool,
        UnsubscribeRequestParams, UpdateTaskParams,
    },
    service::{
        serve_client, serve_client_with_lifecycle, ClientLifecycleMode, NotificationContext,
        PeerRequestOptions, RequestContext, RoleClient, RoleServer, RunningService,
        SubscriptionContext,
    },
    ServerHandler,
};
use tokio::{
    io::DuplexStream,
    sync::{mpsc, Notify},
};

use super::*;

const IO_BUF: usize = 64 * 1024;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const URI: &str = "workspace://notes.md";

fn pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(IO_BUF)
}

fn auto_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        legacy_version: None,
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wire condition not reached before deadline");
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

fn internal(context: &str, error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(format!("{context}: {error}"), None)
}

/// 上游 server 关闭必须有界；subscription/长任务不会阻塞测试收尾。
async fn stop_server(mut server: RunningService<RoleServer, MockUpstream>) {
    let _ = server.close_with_timeout(Duration::from_millis(500)).await;
}

// ─── 下游记录 handler ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Recorder {
    state: Arc<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    progress: Mutex<Vec<ProgressNotificationParam>>,
    resource_updates: Mutex<Vec<String>>,
    task_status: Mutex<Vec<String>>,
    tool_list_changed: AtomicUsize,
    logging: Mutex<Vec<String>>,
    custom: Mutex<Vec<String>>,
}

impl Recorder {
    fn progress(&self) -> Vec<ProgressNotificationParam> {
        self.state.progress.lock().clone()
    }

    fn resource_updates(&self) -> Vec<String> {
        self.state.resource_updates.lock().clone()
    }

    fn task_status(&self) -> Vec<String> {
        self.state.task_status.lock().clone()
    }

    fn tool_list_changed(&self) -> usize {
        self.state.tool_list_changed.load(Ordering::SeqCst)
    }

    fn logging(&self) -> Vec<String> {
        self.state.logging.lock().clone()
    }

    fn custom(&self) -> Vec<String> {
        self.state.custom.lock().clone()
    }
}

impl ClientHandler for Recorder {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::new("upstream-proxy-wire-test", "0.1.0"),
        )
    }

    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.progress.lock().push(params);
        std::future::ready(())
    }

    fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.resource_updates.lock().push(params.uri);
        std::future::ready(())
    }

    fn on_task_status(
        &self,
        params: TaskStatusNotificationParams,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state
            .task_status
            .lock()
            .push(params.task.task.task_id.clone());
        std::future::ready(())
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.tool_list_changed.fetch_add(1, Ordering::SeqCst);
        std::future::ready(())
    }

    #[allow(deprecated)]
    fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.logging.lock().push(params.data.to_string());
        std::future::ready(())
    }

    fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.custom.lock().push(notification.method);
        std::future::ready(())
    }
}

// ─── 上游 mock server ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct MockUpstream {
    state: Arc<MockState>,
}

#[derive(Default)]
struct MockState {
    calls: Mutex<Vec<RecordedCall>>,
    subscriptions: Mutex<Vec<String>>,
    unsubscriptions: Mutex<Vec<String>>,
    cancels: Mutex<Vec<CancelledNotificationParam>>,
    task_gets: Mutex<Vec<(String, RequestMetaObject)>>,
    task_cancels: Mutex<Vec<(String, RequestMetaObject)>>,
    task_updates: Mutex<Vec<(String, RequestMetaObject)>>,
    list_tool_meta: Mutex<Option<RequestMetaObject>>,
    custom_requests: Mutex<Vec<(String, RequestMetaObject)>>,
    listens: Mutex<Vec<SubscriptionFilter>>,
    subscribe_failures: AtomicUsize,
    unsubscribe_failures: AtomicUsize,
    subscribe_delay_ms: AtomicU64,
    started_tx: Mutex<Option<mpsc::UnboundedSender<(RequestId, String)>>>,
    release: Arc<Notify>,
}

#[derive(Clone)]
struct RecordedCall {
    id: RequestId,
    name: String,
    meta: RequestMetaObject,
}

impl MockUpstream {
    fn new() -> Self {
        Self::default()
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.state.calls.lock().clone()
    }

    fn subscriptions(&self) -> Vec<String> {
        self.state.subscriptions.lock().clone()
    }

    fn unsubscriptions(&self) -> Vec<String> {
        self.state.unsubscriptions.lock().clone()
    }

    fn cancels(&self) -> Vec<CancelledNotificationParam> {
        self.state.cancels.lock().clone()
    }

    fn task_gets(&self) -> Vec<(String, RequestMetaObject)> {
        self.state.task_gets.lock().clone()
    }

    fn task_cancels(&self) -> Vec<(String, RequestMetaObject)> {
        self.state.task_cancels.lock().clone()
    }

    fn task_updates(&self) -> Vec<(String, RequestMetaObject)> {
        self.state.task_updates.lock().clone()
    }

    fn list_tool_meta(&self) -> Option<RequestMetaObject> {
        self.state.list_tool_meta.lock().clone()
    }

    fn custom_requests(&self) -> Vec<(String, RequestMetaObject)> {
        self.state.custom_requests.lock().clone()
    }

    fn fail_next_subscribes(&self, count: usize) {
        self.state.subscribe_failures.store(count, Ordering::SeqCst);
    }

    fn fail_next_unsubscribes(&self, count: usize) {
        self.state
            .unsubscribe_failures
            .store(count, Ordering::SeqCst);
    }

    fn delay_subscribes(&self, delay_ms: u64) {
        self.state
            .subscribe_delay_ms
            .store(delay_ms, Ordering::SeqCst);
    }

    fn listens(&self) -> Vec<SubscriptionFilter> {
        self.state.listens.lock().clone()
    }

    fn held_request_channel(&self) -> mpsc::UnboundedReceiver<(RequestId, String)> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.state.started_tx.lock() = Some(tx);
        rx
    }

    fn release_held_call(&self) {
        self.state.release.notify_waiters();
        self.state.release.notify_one();
    }
}

fn capabilities() -> ServerCapabilities {
    let mut capabilities = ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_resources_list_changed()
        .enable_tasks()
        .build();
    if let Some(tools) = capabilities.tools.as_mut() {
        tools.list_changed = Some(true);
    }
    // 上游声明但代理不转发的面：验证 get_info 不会把它们泄漏给下游。
    capabilities.prompts = Some(PromptsCapability::default());
    capabilities
        .extensions
        .get_or_insert_with(ExtensionCapabilities::new)
        .insert(
            peri_acp_types::skills::SKILLS_EXTENSION_ID.to_owned(),
            rmcp::model::JsonObject::new(),
        );
    capabilities
}

fn task_of(id: &str, status: TaskStatus) -> Task {
    Task::new(id, status, "2026-11-11T00:00:00Z", "2026-11-11T00:00:00Z")
}

impl ServerHandler for MockUpstream {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(capabilities())
            .with_server_info(Implementation::new("mock-upstream", "wire-test"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        *self.state.list_tool_meta.lock() = Some(context.meta.clone());
        let tools = [
            "echo",
            "progress",
            "task",
            "fail",
            "tool_error",
            "hold",
            "notify",
            "slow",
        ]
        .into_iter()
        .map(|name| Tool::new(name, format!("{name} tool"), rmcp::model::JsonObject::new()))
        .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    #[allow(deprecated)]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        self.state.calls.lock().push(RecordedCall {
            id: context.id.clone(),
            name: name.clone(),
            meta: context.meta.clone(),
        });
        match name.as_str() {
            "echo" => {
                let arguments = request.arguments.unwrap_or_default();
                let value = arguments
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let delay = arguments
                    .get("delay_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(delay)).await;
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text(value),
                ])))
            }
            "progress" => {
                let token = context.meta.get_progress_token().unwrap_or_else(|| {
                    ProgressToken(rmcp::model::NumberOrString::String("upstream-only".into()))
                });
                context
                    .peer
                    .notify_progress(ProgressNotificationParam::new(token, 1.0).with_message("one"))
                    .await
                    .map_err(|error| internal("progress", error))?;
                // 未注册映射的 token 必须被代理丢弃，不能凭空下传。
                context
                    .peer
                    .notify_progress(ProgressNotificationParam::new(
                        ProgressToken(rmcp::model::NumberOrString::String(
                            "unmapped-upstream".into(),
                        )),
                        2.0,
                    ))
                    .await
                    .map_err(|error| internal("progress", error))?;
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text("progress-done"),
                ])))
            }
            "task" => {
                let task = task_of("up-task-1", TaskStatus::Working);
                let working = DetailedTask::new(task.clone(), TaskPayload::Working);
                context
                    .peer
                    .send_notification(ServerNotification::TaskStatusNotification(
                        TaskStatusNotification::new(TaskStatusNotificationParams::new(working)),
                    ))
                    .await
                    .map_err(|error| internal("task notification", error))?;
                Ok(CallToolResponse::Task(CreateTaskResult::new(task)))
            }
            "fail" => Err(McpError::new(
                ErrorCode(-32042),
                "upstream classified failure",
                None,
            )),
            "tool_error" => Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text("visible tool error"),
            ]))),
            "hold" => {
                if let Some(tx) = self.state.started_tx.lock().as_ref() {
                    let slot = request
                        .arguments
                        .as_ref()
                        .and_then(|arguments| arguments.get("slot"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let _ = tx.send((context.id.clone(), slot));
                }
                self.state.release.notified().await;
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text("released"),
                ])))
            }
            "slow" => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text("slow-done"),
                ])))
            }
            "notify" => {
                context
                    .peer
                    .notify_tool_list_changed()
                    .await
                    .map_err(|error| internal("tool list changed", error))?;
                context
                    .peer
                    .notify_logging_message(LoggingMessageNotificationParam::new(
                        LoggingLevel::Info,
                        serde_json::json!({ "message": "hello" }),
                    ))
                    .await
                    .map_err(|error| internal("logging", error))?;
                context
                    .peer
                    .send_notification(ServerNotification::CustomNotification(CustomNotification {
                        method: "workspace/custom".to_owned(),
                        params: Some(serde_json::json!({ "ok": true })),
                        extensions: Default::default(),
                    }))
                    .await
                    .map_err(|error| internal("custom notification", error))?;
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text("notified"),
                ])))
            }
            other => Err(McpError::invalid_params(
                format!("unknown mock tool: {other}"),
                None,
            )),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![Resource::new(
            URI, "notes",
        )]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(
                format!("content:{}", request.uri),
                request.uri,
            )],
        )))
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.state.subscribe_failures.load(Ordering::SeqCst) > 0 {
            self.state.subscribe_failures.fetch_sub(1, Ordering::SeqCst);
            return Err(internal("subscribe", "mock subscribe failure"));
        }
        let delay = self.state.subscribe_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.state.subscriptions.lock().push(request.uri.clone());
        context
            .peer
            .notify_resource_updated(ResourceUpdatedNotificationParam::new(request.uri))
            .await
            .map_err(|error| internal("resource updated", error))?;
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.state.unsubscribe_failures.load(Ordering::SeqCst) > 0 {
            self.state
                .unsubscribe_failures
                .fetch_sub(1, Ordering::SeqCst);
            return Err(internal("unsubscribe", "mock unsubscribe failure"));
        }
        self.state.unsubscriptions.lock().push(request.uri);
        Ok(())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(requested.clone())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        self.state.listens.lock().push(context.requested().clone());
        for uri in context
            .accepted()
            .resource_subscriptions
            .clone()
            .unwrap_or_default()
        {
            context
                .sink()
                .send(ServerNotification::ResourceUpdatedNotification(
                    ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri)),
                ))
                .await
                .map_err(|error| internal("listen resource update", error))?;
        }
        tokio::select! {
            _ = context.cancelled() => Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(30)) => Ok(()),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        self.state
            .task_gets
            .lock()
            .push((request.task_id.clone(), context.meta.clone()));
        let task = task_of(&request.task_id, TaskStatus::Completed);
        Ok(GetTaskResult::new(DetailedTask::new(
            task,
            TaskPayload::Completed {
                result: rmcp::model::JsonObject::new(),
            },
        )))
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.state
            .task_cancels
            .lock()
            .push((request.task_id, context.meta.clone()));
        Ok(())
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.state
            .task_updates
            .lock()
            .push((request.task_id, context.meta.clone()));
        Ok(())
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        self.state
            .custom_requests
            .lock()
            .push((request.method.clone(), context.meta.clone()));
        Ok(CustomResult::new(serde_json::json!({
            "method": request.method,
        })))
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        self.state.cancels.lock().push(notification);
        std::future::ready(())
    }
}

// ─── 链路夹具 ─────────────────────────────────────────────────────────────────

struct Upstream {
    server: RunningService<RoleServer, MockUpstream>,
    generation: UpstreamGeneration,
    /// Legacy client service 没有自动 watcher，由夹具持有以维持链路。
    _client: Option<RunningService<RoleClient, UpstreamProxyClient>>,
}

async fn connect_auto_upstream(proxy: &UpstreamProxy, mock: MockUpstream) -> Upstream {
    let (server_io, client_io) = pair();
    // modern 上游没有 initialize；`serve_directly` 立即返回，discover/请求照常分派。
    let server = rmcp::service::serve_directly(mock, server_io, None);
    let connection = proxy
        .connect_upstream(client_io, TEST_TIMEOUT)
        .await
        .expect("proxy connects upstream");
    Upstream {
        server,
        generation: connection.generation(),
        _client: None,
    }
}

async fn connect_legacy_upstream(proxy: &UpstreamProxy, mock: MockUpstream) -> Upstream {
    let (server_io, client_io) = pair();
    let server_task = tokio::spawn(async move { rmcp::serve_server(mock, server_io).await });
    let (generation, handler) = proxy.new_connection();
    let client = serve_client(handler, client_io)
        .await
        .expect("legacy upstream handshake");
    let server = server_task
        .await
        .expect("server task joins")
        .expect("mock upstream serves");
    proxy
        .attach(generation, client.peer().clone())
        .expect("attach legacy upstream");
    Upstream {
        server,
        generation,
        _client: Some(client),
    }
}

struct Downstream {
    client: RunningService<RoleClient, Recorder>,
    recorder: Recorder,
}

async fn connect_downstream(proxy: &UpstreamProxy, modern: bool) -> Downstream {
    let recorder = Recorder::default();
    let (server_io, client_io) = pair();
    let server_proxy = proxy.clone();
    // 转发 server 的生命周期由测试客户端驱动；句柄保留在后台直到 transport 关闭。
    tokio::spawn(async move {
        if let Ok(service) = rmcp::serve_server(server_proxy, server_io).await {
            let _ = service.waiting().await;
        }
    });
    let client = if modern {
        serve_client_with_lifecycle(recorder.clone(), client_io, auto_lifecycle())
            .await
            .expect("modern downstream handshake")
    } else {
        serve_client(recorder.clone(), client_io)
            .await
            .expect("legacy downstream handshake")
    };
    Downstream { client, recorder }
}

async fn echo(peer: &rmcp::service::Peer<RoleClient>, value: &str, delay_ms: u64) -> String {
    let arguments = serde_json::json!({ "value": value, "delay_ms": delay_ms })
        .as_object()
        .cloned()
        .unwrap_or_default();
    let result = peer
        .call_tool_once(CallToolRequestParams::new("echo").with_arguments(arguments))
        .await
        .expect("echo call");
    match result {
        CallToolResponse::Complete(result) => text_of(&result),
        _ => panic!("echo must complete"),
    }
}

fn owner_meta(owner: &str) -> RequestMetaObject {
    let mut meta = RequestMetaObject::new();
    meta.0 .0.insert(
        peri_mcp_core::task_owner::TASK_OWNER_META_KEY.to_owned(),
        serde_json::Value::String(owner.to_owned()),
    );
    meta
}

// ─── 并发 / 取消 / progress ───────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_calls_keep_ids_and_results_separate() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    let peer = downstream.client.peer().clone();
    let slow = tokio::spawn({
        let peer = peer.clone();
        async move { echo(&peer, "slow-value", 120).await }
    });
    let fast = tokio::spawn({
        let peer = peer.clone();
        async move { echo(&peer, "fast-value", 0).await }
    });
    assert_eq!(fast.await.expect("fast task"), "fast-value");
    assert_eq!(slow.await.expect("slow task"), "slow-value");

    let calls = mock.calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].id, calls[1].id, "并发请求必须使用不同上游 id");
    assert!(calls.iter().all(|call| call.name == "echo"));

    let tools = downstream
        .client
        .peer()
        .list_tools(None)
        .await
        .expect("tools/list");
    assert!(
        tools.tools.iter().any(|tool| tool.name == "echo"),
        "tools/list 必须经代理返回上游工具"
    );

    assert!(proxy.is_ready());
    drop(downstream);
    stop_server(upstream.server).await;
}

#[tokio::test]
async fn downstream_cancel_forwards_the_matching_upstream_request_id() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let mut started = mock.held_request_channel();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    let peer = downstream.client.peer().clone();
    let hold = |slot: &'static str| {
        let peer = peer.clone();
        async move {
            peer.send_cancellable_request(
                ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(
                    CallToolRequestParams::new("hold").with_arguments(
                        serde_json::json!({ "slot": slot })
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    ),
                )),
                PeerRequestOptions::no_options(),
            )
            .await
            .expect("hold request starts")
        }
    };
    let first = hold("first").await;
    let second = hold("second").await;
    let mut ids = std::collections::HashMap::new();
    for _ in 0..2 {
        let (id, slot) = tokio::time::timeout(TEST_TIMEOUT, started.recv())
            .await
            .expect("upstream started before deadline")
            .expect("started channel");
        ids.insert(slot, id);
    }
    assert_ne!(ids["first"], ids["second"]);

    first
        .cancel(Some("cancel first".to_owned()))
        .await
        .expect("downstream cancel is sent");
    wait_until(|| mock.cancels().len() == 1).await;
    let cancels = mock.cancels();
    assert_eq!(
        cancels[0].request_id.as_ref(),
        Some(&ids["first"]),
        "取消必须指向被取消调用自己的上游 id"
    );
    assert_ne!(
        cancels[0].request_id.as_ref(),
        Some(&ids["second"]),
        "并发调用之间不得串线"
    );
    assert_eq!(
        cancels[0].reason.as_deref(),
        Some("cancelled by downstream client")
    );

    second
        .cancel(Some("cancel second".to_owned()))
        .await
        .expect("second downstream cancel is sent");
    wait_until(|| mock.cancels().len() == 2).await;
    assert_eq!(mock.cancels()[1].request_id.as_ref(), Some(&ids["second"]));
    assert!(proxy.is_ready(), "取消不是断线，能力保持 ready");

    mock.release_held_call();
    mock.release_held_call();
    stop_server(upstream.server).await;
}

#[tokio::test]
async fn progress_tokens_are_mapped_to_the_downstream_token() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    let handle = downstream
        .client
        .peer()
        .send_cancellable_request(
            ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(
                CallToolRequestParams::new("progress"),
            )),
            PeerRequestOptions::no_options(),
        )
        .await
        .expect("progress request starts");
    let downstream_token = handle.progress_token.clone();
    let result = handle.await_response().await.expect("progress call");
    assert!(matches!(
        result,
        rmcp::model::ServerResult::CallToolResult(_)
    ));

    wait_until(|| downstream.recorder.progress().len() == 1).await;
    let progress = downstream.recorder.progress();
    assert_eq!(progress[0].progress_token, downstream_token);
    assert_eq!(progress[0].progress, 1.0);
    assert_eq!(progress[0].message.as_deref(), Some("one"));

    // 未映射的 progress 已在上游发出；给转发留出时间后仍只能有一条。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        downstream.recorder.progress().len(),
        1,
        "未映射的 progress 必须被丢弃"
    );

    stop_server(upstream.server).await;
}

// ─── task ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn task_ids_are_capability_prefixed_and_mapped_on_every_surface() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    let mut call = CallToolRequestParams::new("task");
    call.meta = Some(owner_meta("session-a"));
    let created = downstream
        .client
        .peer()
        .call_tool_once(call)
        .await
        .expect("task call");
    let task_id = match created {
        CallToolResponse::Task(created) => created.task.task_id,
        _ => panic!("task tool must return CreateTaskResult"),
    };
    assert!(
        task_id.starts_with("workspace:task:"),
        "task id 必须带 capability 前缀: {task_id}"
    );
    assert_ne!(task_id, "up-task-1");

    wait_until(|| downstream.recorder.task_status().contains(&task_id)).await;
    assert_eq!(
        downstream.recorder.task_status(),
        vec![task_id.clone()],
        "早于响应到达的任务通知必须复用同一个下游 id"
    );

    let mut get = GetTaskParams::new(&task_id);
    get.meta = Some(owner_meta("session-a"));
    let result = downstream
        .client
        .peer()
        .get_task(get)
        .await
        .expect("tasks/get");
    assert_eq!(result.task.task.task_id, task_id);
    assert_eq!(result.task.status(), TaskStatus::Completed);

    let mut cancel = CancelTaskParams::new(&task_id);
    cancel.meta = Some(owner_meta("session-a"));
    downstream
        .client
        .peer()
        .cancel_task(cancel)
        .await
        .expect("tasks/cancel");

    let mut update = UpdateTaskParams::new(&task_id, Default::default());
    update.meta = Some(owner_meta("session-a"));
    downstream
        .client
        .peer()
        .update_task(update)
        .await
        .expect("tasks/update");

    let gets = mock.task_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].0, "up-task-1");
    assert_eq!(
        gets[0]
            .1
             .0
             .0
            .get(peri_mcp_core::task_owner::TASK_OWNER_META_KEY)
            .and_then(serde_json::Value::as_str),
        Some("session-a"),
        "task owner 元数据必须原样上行"
    );
    let cancels = mock.task_cancels();
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0].0, "up-task-1");

    let updates = mock.task_updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, "up-task-1");
    assert_eq!(
        updates[0]
            .1
             .0
             .0
            .get(peri_mcp_core::task_owner::TASK_OWNER_META_KEY)
            .and_then(serde_json::Value::as_str),
        Some("session-a"),
        "tasks/update 的 owner 元数据必须原样上行"
    );

    let call_owner = mock.calls()[0]
        .meta
        .0
         .0
        .get(peri_mcp_core::task_owner::TASK_OWNER_META_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    assert_eq!(call_owner.as_deref(), Some("session-a"));

    let foreign = GetTaskParams::new("workspace:task:unknown");
    let error = downstream
        .client
        .peer()
        .get_task(foreign)
        .await
        .expect_err("unknown task id must fail");
    assert!(matches!(
        error,
        rmcp::ServiceError::McpError(error) if error.code == ErrorCode::INVALID_PARAMS
    ));
    assert_eq!(mock.task_gets().len(), 1, "未知 task id 不得上行");

    stop_server(upstream.server).await;
}

#[path = "upstream_proxy_wire_test/surfaces.rs"]
mod surfaces;

#[path = "upstream_proxy_wire_test/resilience.rs"]
mod resilience;
