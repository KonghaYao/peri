//! 通用 MCP 上游代理（v5 单出口 P2）：一个 capability 的 client → server 转发。
//!
//! 用途：单出口 router 聚合 `workspace` 等上游 server 时，把下游 MCP 请求映射到
//! 一条上游 client 连接，并保持请求关联、取消、progress、task id、资源 URI 与
//! 通知语义。模块当前**不接入默认装配**；每个实例只服务一个上游连接与一个下游
//! 会话，能力失败只降级本实例（健康状态见 [`CapabilityHealth`]）。
//!
//! 边界：
//! - 不做工具/资源改名，不做跨版本订阅仿真（legacy `resources/subscribe` 与
//!   `subscriptions/listen` 均按上游协商结果直接转发）；
//! - 不接管上游生命周期；调用方用 [`UpstreamProxy::attach`] 或
//!   [`UpstreamProxy::connect_upstream`] 装配连接，断线经 health 暴露；
//! - upstream 返回的 JSON-RPC 错误原样上抛，只有传输层故障才收敛为本能力不可用。

use std::{future::Future, sync::Arc, time::Duration};

#[allow(deprecated)]
use rmcp::model::LoggingMessageNotificationParam;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientRequest, CustomNotification, CustomRequest, CustomResult, ErrorData as McpError,
        ExtensionCapabilities, GetMeta, GetTaskParams, GetTaskResult, Implementation,
        InitializeRequestParams, InitializeResult, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, NotificationMetaObject, PaginatedRequestParams,
        ProgressNotification, ProgressNotificationParam, PromptListChangedNotification,
        ReadResourceRequestParams, ReadResourceResponse, RequestMetaObject,
        ResourceListChangedNotification, ResourceUpdatedNotification,
        ResourceUpdatedNotificationParam, ServerCapabilities, ServerConfig, ServerNotification,
        ServerResult, SubscribeRequestParams, SubscriptionFilter, TaskStatusNotification,
        TaskStatusNotificationParams, ToolListChangedNotification, UnsubscribeRequestParams,
        UpdateTaskParams, TASKS_EXTENSION_ID,
    },
    service::{
        serve_client_with_lifecycle_and_ct, ClientInitializeError, ClientLifecycleMode,
        MaybeSendFuture, NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleClient,
        RoleServer, SubscriptionContext,
    },
    transport::IntoTransport,
    ClientHandler, ServerHandler,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod state;

use state::ProxyInner;

/// 上游连接代次。重连生成新代次；旧代次的 task 与通知一律失效。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UpstreamGeneration(u64);

impl UpstreamGeneration {
    pub fn value(self) -> u64 {
        self.0
    }
}

/// 上游连接丢失原因（供 router / readiness 展示 warning）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamLossReason {
    /// 传输关闭或发送失败。
    TransportClosed,
    /// 连接服务退出（含主动关闭）。
    ServiceEnded,
    /// 握手失败后没有可用连接。
    HandshakeFailed,
}

/// capability 级健康状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityHealth {
    /// 尚未装配上游（首次连接或重连中）。
    Connecting,
    /// 上游已就绪。
    Ready,
    /// 上游不可用；router 应隐藏该能力并把 warning 映射到 readiness。
    Degraded { reason: UpstreamLossReason },
}

impl CapabilityHealth {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// 握手/装配错误。
#[derive(Debug, Error)]
pub enum UpstreamConnectError {
    #[error("upstream handshake timed out")]
    Timeout,
    #[error(transparent)]
    Initialize(Box<ClientInitializeError>),
    #[error(transparent)]
    Attach(#[from] UpstreamAttachError),
}

impl From<ClientInitializeError> for UpstreamConnectError {
    fn from(error: ClientInitializeError) -> Self {
        Self::Initialize(Box::new(error))
    }
}

/// 旧代次连接不得覆盖当前代次。
#[derive(Debug, Error)]
pub enum UpstreamAttachError {
    #[error(
        "stale upstream generation {generation:?} (latest allocated {latest:?})",
        generation = .generation,
        latest = .latest
    )]
    StaleGeneration {
        generation: UpstreamGeneration,
        latest: UpstreamGeneration,
    },
}

/// 代理句柄。`Clone` 共享同一份状态，可交给 `rmcp::serve_server`。
#[derive(Clone)]
pub struct UpstreamProxy {
    inner: Arc<ProxyInner>,
}

/// 上游 client handler：把上游通知转发到当前下游 peer，并做 task/progress 映射。
pub struct UpstreamProxyClient {
    inner: Arc<ProxyInner>,
    generation: UpstreamGeneration,
}

/// 已装配的上游连接。
pub struct UpstreamConnection {
    generation: UpstreamGeneration,
    peer: Peer<RoleClient>,
    shutdown: CancellationToken,
}

impl UpstreamConnection {
    pub fn generation(&self) -> UpstreamGeneration {
        self.generation
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        &self.peer
    }

    /// 主动关闭上游；服务退出后 health 转为 `Degraded`。
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

impl UpstreamProxy {
    /// 创建未装配上游的代理。`capability` 用于 task id 前缀与诊断日志。
    pub fn new(capability: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(ProxyInner::new(capability.into())),
        }
    }

    /// 配置单次上游请求的兜底超时；超时只取消该次请求，不降级能力。
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("proxy state is not shared yet")
            .request_timeout = Some(timeout);
        self
    }

    pub fn capability(&self) -> &str {
        &self.inner.capability
    }

    pub fn health(&self) -> CapabilityHealth {
        self.inner.health.borrow().clone()
    }

    pub fn subscribe_health(&self) -> watch::Receiver<CapabilityHealth> {
        self.inner.health.subscribe()
    }

    pub fn is_ready(&self) -> bool {
        self.health().is_ready()
    }

    /// 分配一个新代次及配套 client handler；握手成功后调用 [`Self::attach`]。
    pub fn new_connection(&self) -> (UpstreamGeneration, UpstreamProxyClient) {
        let generation = self.inner.allocate_generation();
        (
            generation,
            UpstreamProxyClient {
                inner: Arc::clone(&self.inner),
                generation,
            },
        )
    }

    /// 装配已完成握手的上游 peer。旧代次连接会被拒绝，避免重连竞态回退。
    pub fn attach(
        &self,
        generation: UpstreamGeneration,
        peer: Peer<RoleClient>,
    ) -> Result<(), UpstreamAttachError> {
        self.inner.attach(generation, peer)
    }

    /// 标记一条上游连接丢失；仅当它是当前代次（或尚未装配的最新代次）时降级。
    pub fn mark_lost(&self, generation: UpstreamGeneration, reason: UpstreamLossReason) {
        self.inner.mark_lost(generation, reason);
    }

    /// 用生产同款 Auto 协商连接上游，并在服务退出时自动降级。
    pub async fn connect_upstream<T, E, A>(
        &self,
        transport: T,
        timeout: Duration,
    ) -> Result<UpstreamConnection, UpstreamConnectError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        };
        self.connect_upstream_with_lifecycle(transport, lifecycle, timeout)
            .await
    }

    /// 指定 client lifecycle 的连接入口（测试 legacy 订阅语义时使用）。
    pub async fn connect_upstream_with_lifecycle<T, E, A>(
        &self,
        transport: T,
        lifecycle: ClientLifecycleMode,
        timeout: Duration,
    ) -> Result<UpstreamConnection, UpstreamConnectError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let (generation, handler) = self.new_connection();
        let shutdown = CancellationToken::new();
        let handshake =
            serve_client_with_lifecycle_and_ct(handler, transport, lifecycle, shutdown.clone());
        let service = match tokio::time::timeout(timeout, handshake).await {
            Ok(Ok(service)) => service,
            Ok(Err(error)) => {
                shutdown.cancel();
                self.inner
                    .mark_lost(generation, UpstreamLossReason::HandshakeFailed);
                return Err(error.into());
            }
            Err(_) => {
                shutdown.cancel();
                self.inner
                    .mark_lost(generation, UpstreamLossReason::HandshakeFailed);
                return Err(UpstreamConnectError::Timeout);
            }
        };
        let peer = service.peer().clone();
        if let Err(error) = self.attach(generation, peer.clone()) {
            let _ = service.cancel().await;
            return Err(error.into());
        }
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _ = service.waiting().await;
            inner.mark_lost(generation, UpstreamLossReason::ServiceEnded);
        });
        Ok(UpstreamConnection {
            generation,
            peer,
            shutdown,
        })
    }

    fn merge_request_meta(
        context: &RequestContext<RoleServer>,
        existing: Option<RequestMetaObject>,
    ) -> Option<RequestMetaObject> {
        let mut merged = context.meta.clone();
        if let Some(existing) = existing {
            merged.0 .0.extend(existing.0 .0);
        }
        (!merged.0 .0.is_empty()).then_some(merged)
    }

    fn merge_paginated_meta(
        context: &RequestContext<RoleServer>,
        request: Option<PaginatedRequestParams>,
    ) -> Option<PaginatedRequestParams> {
        match request {
            Some(mut request) => {
                request.meta = Self::merge_request_meta(context, request.meta.take());
                Some(request)
            }
            None if !context.meta.0 .0.is_empty() => {
                let mut request = PaginatedRequestParams::default();
                request.meta = Some(context.meta.clone());
                Some(request)
            }
            None => None,
        }
    }

    /// 在固定上游代次上发送一个可取消请求。
    async fn forward_on(
        &self,
        context: &RequestContext<RoleServer>,
        generation: UpstreamGeneration,
        peer: Peer<RoleClient>,
        request: ClientRequest,
    ) -> Result<ServerResult, McpError> {
        let handle = peer
            .send_request_with_option(request, PeerRequestOptions::no_options())
            .await
            .map_err(|error| self.inner.map_service_error(generation, error))?;
        let upstream_id = handle.id.clone();
        let upstream_progress = handle.progress_token.clone();
        let pending_progress = match context.meta.get_progress_token() {
            Some(token) => {
                self.inner
                    .register_progress(generation, upstream_progress.clone(), token)
            }
            None => Vec::new(),
        };
        for params in pending_progress {
            self.inner
                .send_downstream(
                    generation,
                    ServerNotification::ProgressNotification(ProgressNotification::new(params)),
                )
                .await;
        }
        let timeout = self.inner.request_timeout;
        let response = tokio::select! {
            biased;
            result = handle.await_response() => {
                result.map_err(|error| self.inner.map_service_error(generation, error))
            }
            _ = context.ct.cancelled() => {
                self.inner.notify_upstream_cancelled(
                    &peer,
                    upstream_id,
                    "cancelled by downstream client",
                ).await;
                Err(McpError::internal_error(
                    format!("{} upstream request cancelled by downstream", self.inner.capability),
                    None,
                ))
            }
            _ = async {
                match timeout {
                    Some(timeout) => tokio::time::sleep(timeout).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                self.inner.notify_upstream_cancelled(
                    &peer,
                    upstream_id,
                    "upstream request timed out",
                ).await;
                Err(McpError::internal_error(
                    format!("{} upstream request timed out", self.inner.capability),
                    None,
                ))
            }
        };
        self.inner
            .unregister_progress(generation, &upstream_progress);
        response
    }

    async fn forward(
        &self,
        context: &RequestContext<RoleServer>,
        build: impl FnOnce(UpstreamGeneration) -> Result<ClientRequest, McpError>,
    ) -> Result<(UpstreamGeneration, ServerResult), McpError> {
        let (generation, peer) = self.inner.upstream()?;
        let request = build(generation)?;
        let response = self.forward_on(context, generation, peer, request).await?;
        Ok((generation, response))
    }

    fn unexpected(operation: &str) -> McpError {
        McpError::internal_error(
            format!("upstream returned an unexpected result for {operation}"),
            None,
        )
    }
}

impl ServerHandler for UpstreamProxy {
    fn get_info(&self) -> ServerConfig {
        match self.inner.current_info() {
            Some(info) => {
                let mut config = InitializeResult::new(forwarded_capabilities(&info.capabilities));
                config.protocol_version = info.protocol_version.clone();
                config.server_info = info
                    .server_info
                    .clone()
                    .unwrap_or_else(Implementation::from_build_env);
                config.instructions = info.instructions.clone();
                config
            }
            None => InitializeResult::new(ServerCapabilities::default()),
        }
    }

    fn on_initialized(
        &self,
        context: NotificationContext<RoleServer>,
    ) -> impl std::future::Future<Output = ()> + MaybeSendFuture + '_ {
        self.inner.remember_downstream(&context.peer);
        std::future::ready(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.inner.remember_downstream(&context.peer);
        let request = Self::merge_paginated_meta(&context, request);
        let (_, response) = self
            .forward(&context, |_| {
                Ok(ClientRequest::ListToolsRequest(match request {
                    Some(request) => rmcp::model::ListToolsRequest::with_param(request),
                    None => rmcp::model::ListToolsRequest::default(),
                }))
            })
            .await?;
        match response {
            ServerResult::ListToolsResult(result) => Ok(result),
            _ => Err(Self::unexpected("tools/list")),
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.inner.remember_downstream(&context.peer);
        let mut params = request;
        params.meta = Self::merge_request_meta(&context, params.meta.take());
        let (generation, response) = self
            .forward(&context, |_| {
                Ok(ClientRequest::CallToolRequest(
                    rmcp::model::CallToolRequest::new(params),
                ))
            })
            .await?;
        match response {
            ServerResult::CallToolResult(result) => Ok(CallToolResponse::Complete(result)),
            ServerResult::InputRequiredResult(result) => {
                Ok(CallToolResponse::InputRequired(result))
            }
            ServerResult::CreateTaskResult(mut result) => {
                self.inner.map_create_task(generation, &mut result);
                Ok(CallToolResponse::Task(result))
            }
            _ => Err(Self::unexpected("tools/call")),
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.inner.remember_downstream(&context.peer);
        let request = Self::merge_paginated_meta(&context, request);
        let (_, response) = self
            .forward(&context, |_| {
                Ok(ClientRequest::ListResourcesRequest(match request {
                    Some(request) => rmcp::model::ListResourcesRequest::with_param(request),
                    None => rmcp::model::ListResourcesRequest::default(),
                }))
            })
            .await?;
        match response {
            ServerResult::ListResourcesResult(result) => Ok(result),
            _ => Err(Self::unexpected("resources/list")),
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.inner.remember_downstream(&context.peer);
        let request = Self::merge_paginated_meta(&context, request);
        let (_, response) = self
            .forward(&context, |_| {
                Ok(ClientRequest::ListResourceTemplatesRequest(match request {
                    Some(request) => rmcp::model::ListResourceTemplatesRequest::with_param(request),
                    None => rmcp::model::ListResourceTemplatesRequest::default(),
                }))
            })
            .await?;
        match response {
            ServerResult::ListResourceTemplatesResult(result) => Ok(result),
            _ => Err(Self::unexpected("resources/templates/list")),
        }
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.inner.remember_downstream(&context.peer);
        let mut params = request;
        params.meta = Self::merge_request_meta(&context, params.meta.take());
        let (_, response) = self
            .forward(&context, |_| {
                Ok(ClientRequest::ReadResourceRequest(
                    rmcp::model::ReadResourceRequest::new(params),
                ))
            })
            .await?;
        match response {
            ServerResult::ReadResourceResult(result) => Ok(ReadResourceResponse::Complete(result)),
            ServerResult::InputRequiredResult(result) => {
                Ok(ReadResourceResponse::InputRequired(result))
            }
            _ => Err(Self::unexpected("resources/read")),
        }
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.inner.remember_downstream(&context.peer);
        let uri = request.uri.clone();
        let (generation, peer) = self.inner.upstream()?;
        let _gate = self.inner.subscription_lock(generation).await?;
        if !self.inner.acquire_subscription(generation, &uri)? {
            return Ok(());
        }
        let mut params = request;
        params.meta = Self::merge_request_meta(&context, params.meta.take());
        let response = self
            .forward_on(
                &context,
                generation,
                peer,
                ClientRequest::SubscribeRequest(rmcp::model::SubscribeRequest::new(params)),
            )
            .await;
        if response.is_err() {
            self.inner.rollback_subscription(generation, &uri);
        }
        response.map(|_| ())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.inner.remember_downstream(&context.peer);
        let uri = request.uri.clone();
        let (generation, peer) = self.inner.upstream()?;
        let _gate = self.inner.subscription_lock(generation).await?;
        if !self.inner.release_subscription(generation, &uri)? {
            return Ok(());
        }
        let mut params = request;
        params.meta = Self::merge_request_meta(&context, params.meta.take());
        let response = self
            .forward_on(
                &context,
                generation,
                peer,
                ClientRequest::UnsubscribeRequest(rmcp::model::UnsubscribeRequest::new(params)),
            )
            .await;
        if response.is_err() {
            self.inner.restore_subscription(generation, &uri);
        }
        response.map(|_| ())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let info = self.inner.current_info()?;
        Some(requested.supported_by(&info.capabilities))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        self.inner
            .remember_downstream(&context.request_context().peer);
        let (generation, peer) = self.inner.upstream()?;
        let mut filter = context.accepted().clone();
        if let Some(task_ids) = &filter.task_ids {
            let mapped: Vec<String> = task_ids
                .iter()
                .filter_map(|id| self.inner.resolve_task_id(generation, id).ok())
                .collect();
            filter.task_ids = (!mapped.is_empty()).then_some(mapped);
        }
        let mut subscription = peer
            .listen(filter)
            .await
            .map_err(|error| self.inner.map_service_error(generation, error))?;
        loop {
            tokio::select! {
                biased;
                _ = context.cancelled() => {
                    let _ = subscription.cancel().await;
                    return Ok(());
                }
                next = subscription.next() => {
                    match next {
                        Ok(Some(mut notification)) => {
                            if !self.inner.map_task_notification(generation, &mut notification) {
                                continue;
                            }
                            if context.sink().send(notification).await.is_err() {
                                return Ok(());
                            }
                        }
                        Ok(None) => return Ok(()),
                        Err(error) => {
                            return Err(self.inner.map_service_error(generation, error));
                        }
                    }
                }
            }
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        self.inner.remember_downstream(&context.peer);
        let (generation, response) = self
            .forward(&context, |generation| {
                let task_id = self.inner.resolve_task_id(generation, &request.task_id)?;
                let mut params = request.clone();
                params.task_id = task_id;
                params.meta = Self::merge_request_meta(&context, params.meta.take());
                Ok(ClientRequest::GetTaskRequest(
                    rmcp::model::GetTaskRequest::new(params),
                ))
            })
            .await?;
        match response {
            ServerResult::GetTaskResult(mut result) => {
                self.inner.map_get_task(generation, &mut result);
                Ok(result)
            }
            _ => Err(Self::unexpected("tasks/get")),
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.inner.remember_downstream(&context.peer);
        let (_, response) = self
            .forward(&context, |generation| {
                let task_id = self.inner.resolve_task_id(generation, &request.task_id)?;
                let mut params = request.clone();
                params.task_id = task_id;
                params.meta = Self::merge_request_meta(&context, params.meta.take());
                Ok(ClientRequest::CancelTaskRequest(
                    rmcp::model::CancelTaskRequest::new(params),
                ))
            })
            .await?;
        match response {
            ServerResult::TaskAckResult(_) | ServerResult::EmptyResult(_) => Ok(()),
            _ => Err(Self::unexpected("tasks/cancel")),
        }
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.inner.remember_downstream(&context.peer);
        let (_, response) = self
            .forward(&context, |generation| {
                let task_id = self.inner.resolve_task_id(generation, &request.task_id)?;
                let mut params = request.clone();
                params.task_id = task_id;
                params.meta = Self::merge_request_meta(&context, params.meta.take());
                Ok(ClientRequest::UpdateTaskRequest(
                    rmcp::model::UpdateTaskRequest::new(params),
                ))
            })
            .await?;
        match response {
            ServerResult::TaskAckResult(_) | ServerResult::EmptyResult(_) => Ok(()),
            _ => Err(Self::unexpected("tasks/update")),
        }
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        self.inner.remember_downstream(&context.peer);
        let mut request = request;
        let existing = request.get_meta().clone();
        let existing = (!existing.0 .0.is_empty()).then_some(existing);
        if let Some(merged) = Self::merge_request_meta(&context, existing) {
            request.get_meta_mut().0 .0.extend(merged.0 .0);
        }
        let (_, response) = self
            .forward(&context, |_| Ok(ClientRequest::CustomRequest(request)))
            .await?;
        match response {
            ServerResult::CustomResult(result) => Ok(result),
            _ => Err(Self::unexpected("custom request")),
        }
    }
}

impl ClientHandler for UpstreamProxyClient {
    fn get_info(&self) -> InitializeRequestParams {
        let capabilities = ClientCapabilities::builder().enable_tasks().build();
        debug_assert!(capabilities.supports_tasks());
        InitializeRequestParams::new(
            capabilities,
            Implementation::new("peri-upstream-proxy", env!("CARGO_PKG_VERSION")),
        )
    }

    #[allow(clippy::manual_async_fn)]
    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        async move {
            if !self.inner.is_current(self.generation) {
                return;
            }
            let Some(mut params) = self.inner.route_progress(self.generation, params) else {
                return;
            };
            params.meta = Some(context.meta);
            let notification =
                ServerNotification::ProgressNotification(ProgressNotification::new(params));
            self.inner
                .send_downstream(self.generation, notification)
                .await;
        }
    }

    #[allow(deprecated)]
    fn on_logging_message(
        &self,
        mut params: LoggingMessageNotificationParam,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        params.meta = Some(context.meta);
        let notification = ServerNotification::LoggingMessageNotification(
            rmcp::model::LoggingMessageNotification::new(params),
        );
        async move {
            self.inner
                .send_downstream(self.generation, notification)
                .await
        }
    }

    fn on_resource_updated(
        &self,
        mut params: ResourceUpdatedNotificationParam,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        params.meta = Some(context.meta);
        let notification = ServerNotification::ResourceUpdatedNotification(
            ResourceUpdatedNotification::new(params),
        );
        async move {
            self.inner
                .send_downstream(self.generation, notification)
                .await
        }
    }

    fn on_resource_list_changed(
        &self,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let notification =
            with_notification_meta(ResourceListChangedNotification::default(), context.meta);
        let notification = ServerNotification::ResourceListChangedNotification(notification);
        async move {
            self.inner
                .send_downstream(self.generation, notification)
                .await
        }
    }

    fn on_tool_list_changed(
        &self,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let notification =
            with_notification_meta(ToolListChangedNotification::default(), context.meta);
        let notification = ServerNotification::ToolListChangedNotification(notification);
        async move {
            self.inner
                .send_downstream(self.generation, notification)
                .await
        }
    }

    fn on_prompt_list_changed(
        &self,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let notification =
            with_notification_meta(PromptListChangedNotification::default(), context.meta);
        let notification = ServerNotification::PromptListChangedNotification(notification);
        async move {
            self.inner
                .send_downstream(self.generation, notification)
                .await
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn on_task_status(
        &self,
        mut params: TaskStatusNotificationParams,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        async move {
            params.meta = Some(context.meta);
            let mut notification =
                ServerNotification::TaskStatusNotification(TaskStatusNotification::new(params));
            if !self
                .inner
                .map_task_notification(self.generation, &mut notification)
            {
                return;
            }
            self.inner
                .send_downstream(self.generation, notification)
                .await;
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        async move {
            if !self.inner.is_current(self.generation) {
                return;
            }
            let notification = ServerNotification::CustomNotification(CustomNotification {
                method: notification.method,
                params: notification.params,
                extensions: notification_extensions(context.meta),
            });
            self.inner
                .send_downstream(self.generation, notification)
                .await;
        }
    }
}

/// 只声明代理真正转发的面：tools/resources/tasks；prompts/skills 等不转发即不声明。
fn forwarded_capabilities(upstream: &ServerCapabilities) -> ServerCapabilities {
    let mut capabilities = ServerCapabilities::default();
    capabilities.tools = upstream.tools.clone();
    capabilities.resources = upstream.resources.clone();
    capabilities.extensions = upstream.extensions.as_ref().and_then(|extensions| {
        extensions.get(TASKS_EXTENSION_ID).map(|settings| {
            let mut filtered = ExtensionCapabilities::new();
            filtered.insert(TASKS_EXTENSION_ID.to_owned(), settings.clone());
            filtered
        })
    });
    capabilities
}

fn with_notification_meta<M>(
    mut notification: rmcp::model::NotificationNoParam<M>,
    meta: NotificationMetaObject,
) -> rmcp::model::NotificationNoParam<M> {
    notification.extensions = notification_extensions(meta);
    notification
}

fn notification_extensions(meta: NotificationMetaObject) -> rmcp::model::Extensions {
    let mut extensions = rmcp::model::Extensions::new();
    if !meta.0 .0.is_empty() {
        extensions.insert(meta);
    }
    extensions
}

impl std::fmt::Debug for UpstreamProxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamProxy")
            .field("capability", &self.inner.capability)
            .field("health", &self.health())
            .finish()
    }
}

#[cfg(test)]
#[path = "upstream_proxy_wire_test.rs"]
mod wire_tests;
