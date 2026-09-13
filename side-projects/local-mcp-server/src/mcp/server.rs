//! MCP core：stdio 与 Streamable HTTP **共用**的 `ServerHandler`（WP-005）。
//!
//! ## 职责与边界
//!
//! 本模块只做协议：把 MCP 请求翻译成 [`ToolRequest`]，把 [`crate::wire::ToolResponse`] 翻译回
//! MCP 结果。**不执行任何文件系统或进程操作**——唯一的执行入口是注入的
//! [`ToolExecutor`]。单进程形态（D-003）下生产装配把 `runtime::InProcessExecutor` 注入这里，
//! 工具于是在**同一个进程**内执行；"协议层不拥有执行能力"因此是结构事实（本模块连标准库的
//! 文件系统与进程模块都不引用，见 `server_test.rs` 的静态断言），而不是纪律要求。
//!
//! ## 生命周期（R-005）
//!
//! - **modern**（`2026-07-28`）：无握手；每个请求自带 `_meta`，SDK 负责必填键校验
//!   （缺失 → `-32602`）与版本校验（不支持 → `-32022` 且 `data.supported`/`requested`）；
//!   `server/discover` 必须实现；结果带 `resultType: "complete"`。
//! - **legacy**（`2025-11-25`）：`initialize` → `notifications/initialized` 握手，
//!   之后沿用会话语义；`resources/subscribe` 是这一代的订阅方式。
//! - 两个时代**共用同一个 handler**：era 由请求内容决定（SDK 判定），不依赖进程状态。
//!
//! ## 工具面
//!
//! `tools/list` 恰好返回七个工具，条目逐字来自冻结夹具（见 [`crate::mcp::catalog`]）；
//! 别名 `reading` / `Shell` 只参与 `tools/call` 名称解析，不新增条目，也不改变任何
//! Bash 输入字段（Bash 永远只有 `command`/`timeout`/`run_in_background`）。
//!
//! ## 任务状态面
//!
//! 任务状态经标准资源暴露（[`crate::mcp::resources`]）：modern 用 `subscriptions/listen`，
//! legacy 用 `resources/subscribe` + `notifications/resources/updated`。没有注入
//! [`TaskStatusSource`] 时**不声明** `resources` 能力，对应请求返回方法不存在——
//! 能力声明与真实可达面必须一致，不允许"声明了但不可达"。

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use rmcp::model::Extensions;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CancelledNotification,
    CancelledNotificationParam, DiscoverResult, ErrorCode, ErrorData, Implementation,
    InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ResourceContents, ResourceUpdatedNotification, ResourceUpdatedNotificationParam,
    ResourcesCapability, ServerCapabilities, ServerNotification, SubscribeRequestParams,
    SubscriptionFilter, ToolsCapability, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
use rmcp::{model::ServerInfo, ServerHandler};
use tokio_util::sync::CancellationToken;

use crate::auth::principal_from_parts;
use crate::error::code;
use crate::mcp::catalog;
use crate::mcp::error_map;
use crate::mcp::identity::ConnectionIdentity;
use crate::mcp::resources::{
    self, TaskResourceUri, TaskStatusSource, MAX_RESOURCE_SUBSCRIPTIONS, TASK_RESOURCE_MIME_TYPE,
};
use crate::protocol::{era, methods};
use crate::wire::{resolve_tool_name, ToolExecutor, ToolRequest};

/// 本服务声明的协议版本（顺序即 `server/discover` 的 `supportedVersions` 顺序）。
pub const SUPPORTED_VERSIONS: &[ProtocolVersion] =
    &[ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25];

/// `tools/list` 的缓存提示：注册面在同一次构建内是常量。
const TOOLS_LIST_TTL_MS: u64 = 3_600_000;
/// `server/discover` 的缓存提示：发现信息与调用方无关，可跨授权上下文复用。
const DISCOVER_TTL_MS: u64 = 3_600_000;
/// `resources/list` 的缓存提示：任务集合随运行变化，禁止过期缓存。
const RESOURCES_LIST_TTL_MS: u64 = 0;

/// 共享 MCP core。
pub struct SandboxServer {
    /// 七工具的唯一执行入口（生产实现：`runtime::InProcessExecutor`）。
    executor: Arc<dyn ToolExecutor>,
    /// 本条连接的可信身份。
    identity: ConnectionIdentity,
    /// 任务状态来源；未注入时不声明 `resources` 能力。
    tasks: Option<Arc<dyn TaskStatusSource>>,
    /// legacy `resources/subscribe` 的活动订阅（URI → 取消令牌）。
    subscriptions: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl SandboxServer {
    /// 组装一个连接级 handler。
    pub fn new(
        executor: Arc<dyn ToolExecutor>,
        identity: ConnectionIdentity,
        tasks: Option<Arc<dyn TaskStatusSource>>,
    ) -> Self {
        Self {
            executor,
            identity,
            tasks,
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// stdio 连接：身份在连接建立时生成，不可由客户端指定。
    pub fn for_stdio(
        executor: Arc<dyn ToolExecutor>,
        tasks: Option<Arc<dyn TaskStatusSource>>,
    ) -> Self {
        Self::new(executor, ConnectionIdentity::stdio(), tasks)
    }

    /// 本条连接的**兜底**身份。
    ///
    /// 只对 stdio 是权威身份：HTTP 下每条可信连接的主体由传输层逐请求注入
    /// （见 `identity_for`），`self.identity` 只是没有注入时的安全缺省
    /// ——它永远不匹配任何真实任务 owner，因此越权访问只会得到"不存在"。
    pub fn identity(&self) -> &ConnectionIdentity {
        &self.identity
    }

    /// 解析**本条请求**的可信身份。
    ///
    /// HTTP 传输把 [`crate::auth::AuthenticatedPrincipal`] 放进 `http::request::Parts`
    /// 的 extensions（SDK 再把 `Parts` 搬进 MCP 请求 extensions），因此这里能拿到认证层
    /// 已经判定过的 `(principal, client_instance)`。规则：
    ///
    /// - 有注入即以其为准：`clientInfo`/`instanceId` 等调用方自报字段**永不**参与授权；
    /// - 没有注入（stdio，或非 HTTP 传输）→ 使用连接身份。
    ///
    /// 这样"modern 不依赖 transport session"才是结构事实：同一进程内不同连接的请求
    /// 各自携带自己的主体，任务句柄与资源读写都按该主体的 owner 过滤。
    fn identity_for(&self, extensions: &Extensions) -> ConnectionIdentity {
        match extensions
            .get::<http::request::Parts>()
            .and_then(principal_from_parts)
        {
            Some(principal) => {
                ConnectionIdentity::new(principal.principal(), principal.client_instance())
            }
            None => self.identity.clone(),
        }
    }

    /// 服务端实现身份（`serverInfo`）。
    pub fn server_implementation() -> Implementation {
        Implementation::new("local-mcp-server", env!("CARGO_PKG_VERSION"))
    }

    /// 给客户端的用法说明。
    ///
    /// 说明只放在这里：工具条目本身是源契约的逐字投影，任何"MCP 适配"式的改写都会
    /// 破坏与夹具的精确一致（见 `catalog` 的投影规则）。
    pub fn instructions() -> &'static str {
        "Peri 七工具（Read/Write/Edit/Glob/Grep/folder_operations/Bash）的本机 MCP 服务：\
七工具在本进程内以当前用户权限直接在本机执行，**不提供沙箱、容器或网络隔离**；\
文件类工具限定在启动参数指定的工作区根内，而该根是**能力边界而不是安全边界**，\
Bash 以该根为 cwd 执行任意命令。tools/list 固定返回这七个工具；名称别名 reading→Read、\
Shell→Bash 只用于 tools/call 的名称解析，不会出现在列表里。Bash 的输入保持 \
command/timeout/run_in_background 三字段：任务状态经资源 sandbox://tasks（集合）与 \
sandbox://tasks/{task_id}（单个任务）暴露，modern 客户端用 subscriptions/listen 订阅、\
legacy 客户端用 resources/subscribe，变化以 notifications/resources/updated 通知。"
    }

    /// 本服务声明的能力（与真实可达面一致）。
    pub fn capabilities(&self) -> ServerCapabilities {
        let mut capabilities = ServerCapabilities::default();
        // 注册面在一次构建内固定：声明 tools 但不声明 list_changed。
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        capabilities.tools = Some(tools);

        capabilities.resources = self.tasks.as_ref().map(|_| {
            let mut resources = ResourcesCapability::default();
            resources.subscribe = Some(true);
            resources.list_changed = Some(false);
            resources
        });
        capabilities
    }

    /// 取得任务状态来源；未注入时按"方法不存在"处理，而不是伪造空结果。
    fn require_task_source(
        &self,
        method: &'static str,
    ) -> Result<&Arc<dyn TaskStatusSource>, ErrorData> {
        self.tasks
            .as_ref()
            .ok_or_else(|| ErrorData::new(ErrorCode(code::METHOD_NOT_FOUND), method, None))
    }

    fn server_info_meta(&self) -> rmcp::model::MetaObject {
        error_map::meta_with_server_info(None, &Self::server_implementation())
    }
}

/// 资源不存在 / 不属于本连接：统一 `-32602`，`data.uri` 回显调用方提交的 URI。
///
/// `2026-07-28` 起 resource-not-found 归入 `INVALID_PARAMS`（SEP-2164，规范
/// `server/resources#error-handling` 明确 MUST），旧的 `-32002` 在本协议版本**必须不得**
/// 发出（见 [`crate::error::code`]）；"别人的任务"与"不存在的任务"返回同一错误，
/// 避免泄露他人任务是否存在。
fn resource_not_found(uri: &str) -> ErrorData {
    error_map::rpc_error_with_data(
        code::INVALID_PARAMS,
        format!("Resource not found: {uri}"),
        serde_json::json!({ "uri": uri }),
    )
}

/// 资源 URI 形状非法（含走私型输入）：同样是 `-32602`，但不做任何解析回显。
fn invalid_resource_uri(uri: &str) -> ErrorData {
    error_map::rpc_error_with_data(
        code::INVALID_PARAMS,
        format!("Invalid resource URI: {uri}"),
        serde_json::json!({ "uri": uri }),
    )
}

/// 本服务实现的请求方法清单与"未路由请求"归因都在 [`crate::protocol::methods`]：那里是
/// 唯一事实源，协议层只消费它，避免两份清单漂移。
///
/// 分页游标：本服务的列表面是单页（`tools/list` 固定七项、`resources/list` 只列本连接的
/// 任务），**从不**发出 `nextCursor`，因此调用方提交的任何 `cursor` 都必然是无效游标。规范
/// `server/utilities/pagination` 要求 "Invalid cursors SHOULD result in an error with code
/// -32602"，这里据此拒绝，而不是悄悄忽略后返回同一页（那会让调用方以为自己翻到了下一页）。
fn invalid_cursor() -> ErrorData {
    error_map::rpc_error(
        code::INVALID_PARAMS,
        "Invalid cursor: this server is single-page and never issues cursors",
    )
}

/// 从分页参数里取出调用方提交的游标（`Some` 即无效游标，见 [`invalid_cursor`]）。
fn submitted_cursor(request: Option<&PaginatedRequestParams>) -> Option<&str> {
    request.and_then(|params| params.cursor.as_deref())
}

/// 请求是否属于 modern 时代（`>= 2026-07-28`，ISO 日期可按字典序比较）。
///
/// 判定实现在 [`crate::protocol::era`]：它是纯函数，没有会话状态可读，所以"modern 无会话"
/// 是结构事实而不是纪律要求。
fn is_modern(context: &RequestContext<RoleServer>) -> bool {
    // `protocol_version()` 按值返回版本，因此判定在表达式内完成，不把借用带出语句。
    matches!(
        context.protocol_version(),
        Some(version) if era::Era::of(version.as_str()).is_modern()
    )
}

impl ServerHandler for SandboxServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = InitializeResult::new(self.capabilities());
        // 未协商前声明我们优先的版本；`initialize` 的默认实现会按对端请求改写这里。
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.server_info = Self::server_implementation();
        info.instructions = Some(Self::instructions().to_string());
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        // 必须显式声明：`ProtocolVersion::LATEST` 只是 2025-11-25，依赖默认值会让
        // modern 请求被当成不支持。
        Cow::Borrowed(SUPPORTED_VERSIONS)
    }

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, ErrorData> {
        // `from_server_info` 已把 `serverInfo` 写进结果 `_meta`（规范 SHOULD）。
        Ok(
            DiscoverResult::from_server_info(SUPPORTED_VERSIONS.to_vec(), self.get_info())
                .with_ttl_ms(DISCOVER_TTL_MS)
                .with_cache_scope(CacheScope::Public),
        )
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if submitted_cursor(request.as_ref()).is_some() {
            return Err(invalid_cursor());
        }
        let mut result = ListToolsResult::with_all_items(catalog::tools().to_vec());
        result.meta = Some(self.server_info_meta());
        if is_modern(&context) {
            // 注册面在构建期即固定，跨授权上下文也一致，因此可共享缓存。
            result.ttl_ms = Some(TOOLS_LIST_TTL_MS);
            result.cache_scope = Some(CacheScope::Public);
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // 名称解析：规范名或已冻结别名；未知名称是协议错误（-32602），不是工具结果。
        let Some(name) = resolve_tool_name(&request.name) else {
            return Err(error_map::mcp_error(
                &crate::error::ToolError::UnknownTool {
                    name: request.name.to_string(),
                },
            ));
        };

        let arguments = match request.arguments {
            Some(arguments) => serde_json::Value::Object(arguments),
            None => serde_json::Value::Object(serde_json::Map::new()),
        };
        // 身份取自**本次请求**（HTTP：认证层注入的可信主体；stdio：连接身份）。
        let identity = self.identity_for(&context.extensions);
        let tool_request = ToolRequest {
            name,
            arguments,
            // `context.ct` 由 SDK 在收到 notifications/cancelled 或连接关闭时取消，
            // 必须原样传给执行器，避免已取消的调用继续执行。
            context: identity.request_context(context.id.to_string(), context.ct.clone()),
        };

        match self.executor.execute(tool_request).await {
            Ok(response) => Ok(CallToolResponse::Complete(error_map::call_tool_result(
                &response,
                &Self::server_implementation(),
            ))),
            Err(error) => Err(error_map::mcp_error(&error)),
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        if submitted_cursor(request.as_ref()).is_some() {
            return Err(invalid_cursor());
        }
        let source = self.require_task_source("resources/list")?;
        let identity = self.identity_for(&context.extensions);
        let snapshots = source
            .snapshots(identity.principal(), identity.client_instance())
            .await;
        let owned = resources::owned_snapshots(snapshots, &identity);

        let mut items = Vec::with_capacity(owned.len() + 1);
        items.push(resources::collection_resource());
        items.extend(owned.iter().map(resources::task_resource));

        let mut result = ListResourcesResult::with_all_items(items);
        result.meta = Some(self.server_info_meta());
        if is_modern(&context) {
            // 任务集合随运行变化且按主体隔离：不得被任何缓存跨上下文复用。
            result.ttl_ms = Some(RESOURCES_LIST_TTL_MS);
            result.cache_scope = Some(CacheScope::Private);
        }
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let source = self.require_task_source("resources/read")?;
        let uri = request.uri;
        let identity = self.identity_for(&context.extensions);

        let (contents, ttl_ms) = match resources::parse_task_uri(&uri) {
            None => return Err(invalid_resource_uri(&uri)),
            Some(TaskResourceUri::Collection) => {
                let snapshots = source
                    .snapshots(identity.principal(), identity.client_instance())
                    .await;
                let owned = resources::owned_snapshots(snapshots, &identity);
                (resources::collection_payload(&owned), RESOURCES_LIST_TTL_MS)
            }
            Some(TaskResourceUri::Task(task_id)) => {
                let snapshot = source
                    .snapshot(identity.principal(), identity.client_instance(), &task_id)
                    .await;
                // 再次校验所有者：provider 越权返回他人任务时，这里必须挡住。
                match resources::owned_snapshot(snapshot, &identity) {
                    Some(snapshot) => (resources::task_payload(&snapshot), RESOURCES_LIST_TTL_MS),
                    None => return Err(resource_not_found(&uri)),
                }
            }
        };

        let mut result =
            ReadResourceResult::new(vec![ResourceContents::text(contents, uri.clone())
                .with_mime_type(TASK_RESOURCE_MIME_TYPE)]);
        result.meta = Some(self.server_info_meta());
        if is_modern(&context) {
            result.ttl_ms = Some(ttl_ms);
            result.cache_scope = Some(CacheScope::Private);
        }
        Ok(result.into())
    }

    /// modern 订阅过滤器：只接受本命名空间的任务资源 URI。
    ///
    /// 这里做的是**形状**过滤（同步方法无法查询 provider）；所有者判定在
    /// [`Self::listen`] 的每一轮通知前用可信身份再次执行，并在 provider 返回越权快照时
    /// 由 [`resources::owned_snapshot`] 丢弃。
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.tasks.as_ref()?;

        let requested_uris = requested.resource_subscriptions.clone().unwrap_or_default();
        let accepted_uris = requested_uris
            .into_iter()
            .filter(|uri| resources::is_task_resource_uri(uri))
            .take(MAX_RESOURCE_SUBSCRIPTIONS)
            .collect();

        let mut accepted = SubscriptionFilter::new();
        accepted.resource_subscriptions = Some(accepted_uris);
        Some(accepted)
    }

    /// modern `subscriptions/listen`：ack 由 SDK 在进入本方法前发出（带 `subscriptionId`），
    /// 之后按接受集合推送 `notifications/resources/updated`；请求取消即结束订阅。
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let Some(source) = self.tasks.clone() else {
            return Ok(());
        };
        let uris: Vec<String> = context
            .accepted()
            .resource_subscriptions
            .clone()
            .unwrap_or_default();
        if uris.is_empty() {
            // 没有可订阅资源时保持连接直到请求结束，语义与"空订阅"一致。
            context.cancelled().await;
            return Ok(());
        }

        let shutdown = CancellationToken::new();
        // 订阅同样绑定**本条请求**的可信身份：跨连接/跨主体订阅他人任务不可能成立。
        let identity = self.identity_for(&context.request_context().extensions);
        let mut updates =
            resources::spawn_task_update_watcher(source, identity, uris, shutdown.clone());

        let subscription_id = context.sink().id().clone();
        let peer = context.request_context().peer.clone();
        let mut server_teardown = false;

        loop {
            tokio::select! {
                _ = context.cancelled() => break,
                update = updates.recv() => {
                    let Some(uri) = update else {
                        // 轮询任务不再产出（例如 provider 故障）：这是**服务端**主动下线
                        // 订阅流，不是客户端取消。
                        server_teardown = true;
                        break;
                    };
                    let notification = ServerNotification::ResourceUpdatedNotification(
                        ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri)),
                    );
                    if context.sink().send(notification).await.is_err() {
                        break;
                    }
                }
            }
        }
        shutdown.cancel();

        if server_teardown {
            // 规范（basic/patterns/cancellation）：服务端主动下线 `subscriptions/listen`
            // 订阅流时 **MUST** 发送 `notifications/cancelled` 并引用该请求 id。
            // 这里不能用 sink（它按设计拒绝该通知类型），必须走 peer。
            let notification = ServerNotification::CancelledNotification(
                CancelledNotification::new(CancelledNotificationParam::new(
                    Some(subscription_id),
                    Some("subscription stream ended by server".to_string()),
                )),
            );
            if peer.send_notification(notification).await.is_err() {
                tracing::debug!("订阅下线通知发送失败：连接可能已关闭");
            }
        }
        Ok(())
    }

    /// legacy `resources/subscribe`：先做所有者判定，再为该 URI 启动轮询转发任务。
    ///
    /// 该方法是 legacy 专用（`2026-07-28` 用 `subscriptions/listen`），SDK 也因此把它
    /// 标记为 deprecated；这里显式实现它，否则 legacy 客户端没有可用的订阅路径。
    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let source = self.require_task_source("resources/subscribe")?;
        let uri = request.uri;

        let task_id = match resources::parse_task_uri(&uri) {
            Some(TaskResourceUri::Task(task_id)) => task_id,
            Some(TaskResourceUri::Collection) => {
                // 集合本身没有"更新"语义（list_changed 声明为 false），只允许订阅单任务。
                return Err(error_map::rpc_error_with_data(
                    code::INVALID_PARAMS,
                    format!("Resource is not subscribable: {uri}"),
                    serde_json::json!({ "uri": uri }),
                ));
            }
            None => return Err(resource_not_found(&uri)),
        };

        let identity = self.identity_for(&context.extensions);
        let snapshot = source
            .snapshot(identity.principal(), identity.client_instance(), &task_id)
            .await;
        if resources::owned_snapshot(snapshot, &identity).is_none() {
            return Err(resource_not_found(&uri));
        }

        let shutdown = CancellationToken::new();
        {
            // 同一 URI 重复订阅：替换旧令牌并取消旧轮询（幂等，不叠加任务）。
            let mut subscriptions = self.subscriptions.lock();
            if let Some(previous) = subscriptions.insert(uri.clone(), shutdown.clone()) {
                previous.cancel();
            }
        }

        let mut updates =
            resources::spawn_task_update_watcher(source.clone(), identity, vec![uri], shutdown);
        let peer = context.peer.clone();
        tokio::spawn(async move {
            while let Some(uri) = updates.recv().await {
                let notification = ServerNotification::ResourceUpdatedNotification(
                    ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri)),
                );
                // 发送失败即连接已关闭：订阅随连接生命周期结束。
                if peer.send_notification(notification).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// legacy `resources/unsubscribe`：取消并移除订阅；未订阅的 URI 是幂等空操作。
    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let removed = self.subscriptions.lock().remove(&request.uri);
        if let Some(previous) = removed {
            previous.cancel();
        }
        Ok(())
    }

    /// 未被 SDK 类型化解析识别的请求：区分"方法不认识"与"方法认识但形状不合法"。
    async fn on_custom_request(
        &self,
        request: rmcp::model::CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CustomResult, ErrorData> {
        match methods::classify_unrouted(&request.method) {
            // 方法本身存在，因此不是 method-not-found：请求参数不符合该方法的 schema。
            methods::UnroutedRequest::MalformedParams => Err(error_map::rpc_error(
                code::INVALID_PARAMS,
                format!("Invalid params for method {}", request.method),
            )),
            methods::UnroutedRequest::NotImplemented => Err(ErrorData::new(
                ErrorCode(code::METHOD_NOT_FOUND),
                request.method,
                None,
            )),
        }
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
